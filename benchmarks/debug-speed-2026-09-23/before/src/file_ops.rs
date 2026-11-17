use crate::{
    AnalysisBackend, ArchiveStats, CompressOptions, VerifyReport,
    analyzer::analyze,
    archive::{HEADER_LEN, MAGIC, MAX_CHUNKS, ParsedChunk, VERSION, parse_archive},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use memmap2::{Mmap, MmapOptions};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{RecvTimeoutError, sync_channel},
    },
    thread,
    time::Duration,
};

const ANALYSIS_SEGMENT: usize = 64 * 1024 * 1024;
const CHUNK_BATCH: usize = 64;
const DECODE_BATCH: usize = 32;

struct EncodedChunk {
    offset: u64,
    original_len: u32,
    codec: u8,
    checksum: [u8; 32],
    payload: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ZipReport {
    pub entries: usize,
    pub uncompressed_size: u64,
    pub output_directory: Option<PathBuf>,
    pub worker_threads: usize,
}

#[derive(Clone, Debug)]
pub struct ZipCompressionReport {
    pub original_size: u64,
    pub archive_size: u64,
    pub entries: usize,
}

#[derive(Default)]
struct WorkerTracker {
    workers: AtomicU64,
    calling_thread: std::sync::atomic::AtomicBool,
}

impl WorkerTracker {
    fn record(&self) {
        if let Some(index) = rayon::current_thread_index()
            && index < u64::BITS as usize
        {
            self.workers.fetch_or(1u64 << index, Ordering::Relaxed);
        } else {
            self.calling_thread.store(true, Ordering::Relaxed);
        }
    }

    fn count(&self) -> usize {
        self.workers.load(Ordering::Relaxed).count_ones() as usize
            + usize::from(self.calling_thread.load(Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressPhase {
    Analyzing,
    Compressing,
    Decompressing,
    Verifying,
    ZipCompressing,
    ZipExtracting,
    ZipVerifying,
}

#[derive(Clone, Copy, Debug)]
pub struct ProgressInfo {
    pub phase: ProgressPhase,
    pub completed: u64,
    pub total: u64,
}

#[derive(Clone, Debug)]
struct ZipEntryMeta {
    index: usize,
    path: PathBuf,
    size: u64,
    is_dir: bool,
}

/// Compress a file without loading the complete input or output into heap memory.
pub fn compress_file(
    input: &Path,
    output: &Path,
    options: &CompressOptions,
) -> Result<ArchiveStats> {
    compress_file_with_progress(input, output, options, |_| {})
}

pub fn compress_file_with_progress(
    input: &Path,
    output: &Path,
    options: &CompressOptions,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<ArchiveStats> {
    ensure!(
        options.target_chunk_size >= 8192,
        "chunk size must be at least 8192 bytes"
    );
    ensure!(
        options.target_chunk_size <= u32::MAX as usize / 2,
        "chunk size is too large"
    );
    ensure!(
        (0..=22).contains(&options.compression_level),
        "compression level must be between 0 and 22"
    );
    ensure_distinct(input, output)?;

    let input_file = File::open(input)
        .with_context(|| format!("could not open input file {}", input.display()))?;
    let input_size_u64 = input_file.metadata()?.len();
    let input_size =
        usize::try_from(input_size_u64).context("input file is too large for this platform")?;
    let mapping = map_non_empty(&input_file, input_size)?;
    let data = mapping.as_deref().unwrap_or(&[]);
    let (boundaries, backend, whole_hash) =
        segmented_analysis(data, options.target_chunk_size, &mut progress)?;
    let chunk_count = boundaries.len().saturating_sub(1);
    ensure!(
        chunk_count <= MAX_CHUNKS,
        "archive would contain too many chunks"
    );

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let output_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(output)
        .with_context(|| format!("could not create output file {}", output.display()))?;
    let result = (|| -> Result<(u64, usize)> {
        let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, output_file);
        write_header(
            &mut writer,
            input_size_u64,
            chunk_count,
            options.target_chunk_size,
            whole_hash,
        )?;
        let mut raw_chunks = 0usize;
        for batch_start in (0..chunk_count).step_by(CHUNK_BATCH) {
            let batch_end = (batch_start + CHUNK_BATCH).min(chunk_count);
            let encoded: Result<Vec<EncodedChunk>> = (batch_start..batch_end)
                .into_par_iter()
                .map(|index| {
                    encode_fast_chunk(
                        boundaries[index],
                        &data[boundaries[index]..boundaries[index + 1]],
                        options.compression_level,
                    )
                })
                .collect();
            for chunk in encoded? {
                raw_chunks += usize::from(chunk.codec == 0);
                write_chunk(&mut writer, &chunk)?;
            }
            let completed = boundaries[batch_end] as u64;
            progress(ProgressInfo {
                phase: ProgressPhase::Compressing,
                completed,
                total: input_size_u64,
            });
        }
        writer.flush()?;
        let archive_size = writer.stream_position()?;
        Ok((archive_size, raw_chunks))
    })();

    match result {
        Ok((archive_size, raw_chunks)) => Ok(ArchiveStats {
            original_size: input_size_u64,
            archive_size,
            chunk_count,
            backend,
            raw_chunks,
        }),
        Err(error) => {
            let _ = fs::remove_file(output);
            Err(error)
        }
    }
}

/// Decompress a Fastener file in bounded-memory batches.
pub fn decompress_file(input: &Path, output: &Path) -> Result<VerifyReport> {
    decompress_file_with_progress(input, output, |_| {})
}

pub fn decompress_file_with_progress(
    input: &Path,
    output: &Path,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<VerifyReport> {
    ensure_distinct(input, output)?;
    let archive_file =
        File::open(input).with_context(|| format!("could not open archive {}", input.display()))?;
    let archive_size = archive_file.metadata()?.len();
    ensure!(
        archive_size >= HEADER_LEN as u64,
        "file is too short to be a Fastener archive"
    );
    let mapping = map_required(&archive_file)?;
    let parsed = parse_archive(&mapping)?;
    let workers = WorkerTracker::default();
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let output_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(output)
        .with_context(|| format!("could not create restored file {}", output.display()))?;
    let result = (|| -> Result<()> {
        output_file.set_len(parsed.original_size as u64)?;
        if parsed.original_size == 0 {
            return Ok(());
        }
        let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, output_file);
        run_parallel_progress(
            ProgressPhase::Decompressing,
            parsed.original_size as u64,
            &mut progress,
            |completed| {
                for batch in parsed.chunks.chunks(DECODE_BATCH) {
                    let decoded = batch
                        .par_iter()
                        .map(|chunk| {
                            workers.record();
                            let mut output = vec![0u8; chunk.original_len];
                            decode_chunk_into(chunk, &mut output)?;
                            Ok(output)
                        })
                        .collect::<Result<Vec<_>>>()?;
                    for output in decoded {
                        writer.write_all(&output)?;
                        completed.fetch_add(output.len() as u64, Ordering::Relaxed);
                    }
                }
                writer.flush()?;
                Ok(())
            },
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(output);
        return Err(error);
    }
    Ok(VerifyReport {
        original_size: parsed.original_size as u64,
        archive_size,
        chunk_count: parsed.chunks.len(),
        worker_threads: workers.count(),
    })
}

/// Verify a Fastener file without creating a restored file.
pub fn verify_file(input: &Path) -> Result<VerifyReport> {
    verify_file_with_progress(input, |_| {})
}

pub fn verify_file_with_progress(
    input: &Path,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<VerifyReport> {
    let archive_file =
        File::open(input).with_context(|| format!("could not open archive {}", input.display()))?;
    let archive_size = archive_file.metadata()?.len();
    ensure!(
        archive_size >= HEADER_LEN as u64,
        "file is too short to be a Fastener archive"
    );
    let mapping = map_required(&archive_file)?;
    let parsed = parse_archive(&mapping)?;
    let workers = WorkerTracker::default();
    run_parallel_progress(
        ProgressPhase::Verifying,
        parsed.original_size as u64,
        &mut progress,
        |completed| {
            parsed.chunks.par_iter().try_for_each(|chunk| {
                workers.record();
                decode_chunk(chunk)?;
                completed.fetch_add(chunk.original_len as u64, Ordering::Relaxed);
                Ok(())
            })
        },
    )?;
    Ok(VerifyReport {
        original_size: parsed.original_size as u64,
        archive_size,
        chunk_count: parsed.chunks.len(),
        worker_threads: workers.count(),
    })
}

pub fn extract_zip_file(input: &Path, output_directory: &Path) -> Result<ZipReport> {
    extract_zip_file_with_progress(input, output_directory, |_| {})
}

/// Create a conventional single-entry Deflate/Zip64 archive without loading the
/// complete input into memory. The resulting archive is readable by Windows and
/// other standard ZIP implementations.
pub fn compress_zip_file(
    input: &Path,
    output: &Path,
    compression_level: i32,
) -> Result<ZipCompressionReport> {
    compress_zip_file_with_progress(input, output, compression_level, |_| {})
}

pub fn compress_zip_file_with_progress(
    input: &Path,
    output: &Path,
    compression_level: i32,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<ZipCompressionReport> {
    ensure!(
        (0..=9).contains(&compression_level),
        "ZIP compression level must be between 0 and 9"
    );
    ensure_distinct(input, output)?;
    ensure!(input.is_file(), "ZIP input must be one regular file");

    let input_file = File::open(input)
        .with_context(|| format!("could not open input file {}", input.display()))?;
    let original_size = input_file.metadata()?.len();
    let entry_name = input
        .file_name()
        .context("ZIP input path does not have a file name")?
        .to_string_lossy()
        .into_owned();
    let output_file = File::create(output)
        .with_context(|| format!("could not create ZIP {}", output.display()))?;
    let mut archive = zip::ZipWriter::new(BufWriter::with_capacity(8 * 1024 * 1024, output_file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(i64::from(compression_level)))
        .large_file(original_size >= u64::from(u32::MAX));
    archive
        .start_file(entry_name, options)
        .context("could not start ZIP entry")?;

    progress(ProgressInfo {
        phase: ProgressPhase::ZipCompressing,
        completed: 0,
        total: original_size,
    });
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, input_file);
    let mut buffer = vec![0u8; 8 * 1024 * 1024];
    let mut completed = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        archive.write_all(&buffer[..count])?;
        completed += count as u64;
        progress(ProgressInfo {
            phase: ProgressPhase::ZipCompressing,
            completed,
            total: original_size,
        });
    }
    let mut output_writer = archive.finish().context("could not finish ZIP archive")?;
    output_writer.flush()?;
    let archive_size = output_writer.get_ref().metadata()?.len();
    ensure!(
        completed == original_size,
        "input size changed while creating ZIP"
    );

    Ok(ZipCompressionReport {
        original_size,
        archive_size,
        entries: 1,
    })
}

pub fn extract_zip_file_with_progress(
    input: &Path,
    output_directory: &Path,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<ZipReport> {
    ensure!(
        !output_directory.exists(),
        "ZIP output directory already exists: {}",
        output_directory.display()
    );
    let metadata = read_zip_metadata(input)?;
    let uncompressed_size = metadata.iter().map(|entry| entry.size).sum();
    let workers = WorkerTracker::default();
    fs::create_dir_all(output_directory).with_context(|| {
        format!(
            "could not create ZIP output directory {}",
            output_directory.display()
        )
    })?;
    run_parallel_progress(
        ProgressPhase::ZipExtracting,
        uncompressed_size,
        &mut progress,
        |completed| {
            metadata.par_iter().try_for_each_init(
                || open_zip(input),
                |archive, meta| {
                    if !meta.is_dir {
                        workers.record();
                    }
                    let archive = archive
                        .as_mut()
                        .map_err(|error| anyhow!("could not open ZIP worker: {error:#}"))?;
                    extract_zip_entry(archive, meta, output_directory, completed)
                },
            )
        },
    )?;
    Ok(ZipReport {
        entries: metadata.len(),
        uncompressed_size,
        output_directory: Some(output_directory.to_owned()),
        worker_threads: workers.count(),
    })
}

pub fn verify_zip_file(input: &Path) -> Result<ZipReport> {
    verify_zip_file_with_progress(input, |_| {})
}

pub fn verify_zip_file_with_progress(
    input: &Path,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<ZipReport> {
    let metadata = read_zip_metadata(input)?;
    let uncompressed_size = metadata.iter().map(|entry| entry.size).sum();
    let workers = WorkerTracker::default();
    run_parallel_progress(
        ProgressPhase::ZipVerifying,
        uncompressed_size,
        &mut progress,
        |completed| {
            metadata.par_iter().try_for_each_init(
                || open_zip(input),
                |archive, meta| {
                    if !meta.is_dir {
                        workers.record();
                    }
                    let archive = archive
                        .as_mut()
                        .map_err(|error| anyhow!("could not open ZIP worker: {error:#}"))?;
                    verify_zip_entry(archive, meta, completed)
                },
            )
        },
    )?;
    Ok(ZipReport {
        entries: metadata.len(),
        uncompressed_size,
        output_directory: None,
        worker_threads: workers.count(),
    })
}

fn read_zip_metadata(input: &Path) -> Result<Vec<ZipEntryMeta>> {
    let mut archive = open_zip(input)?;
    let mut metadata = Vec::with_capacity(archive.len());
    let mut seen = HashSet::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let path = entry
            .enclosed_name()
            .context("ZIP contains an unsafe path")?
            .to_owned();
        let path_key = path.to_string_lossy().replace('\\', "/").to_lowercase();
        ensure!(seen.insert(path_key), "ZIP contains duplicate output paths");
        let is_symlink = entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000);
        ensure!(!is_symlink, "ZIP symbolic links are not supported");
        metadata.push(ZipEntryMeta {
            index,
            path,
            size: entry.size(),
            is_dir: entry.is_dir(),
        });
    }
    Ok(metadata)
}

fn open_zip(input: &Path) -> Result<zip::ZipArchive<File>> {
    let file =
        File::open(input).with_context(|| format!("could not open ZIP {}", input.display()))?;
    zip::ZipArchive::new(file).context("invalid or unsupported ZIP archive")
}

fn extract_zip_entry(
    archive: &mut zip::ZipArchive<File>,
    meta: &ZipEntryMeta,
    output_directory: &Path,
    completed: &AtomicU64,
) -> Result<()> {
    let mut entry = archive.by_index(meta.index)?;
    ensure!(
        entry.enclosed_name().as_deref() == Some(meta.path.as_path()),
        "ZIP directory changed while extracting"
    );
    let output_path = output_directory.join(&meta.path);
    if meta.is_dir {
        fs::create_dir_all(&output_path)?;
        return Ok(());
    }
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let output_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&output_path)
        .with_context(|| format!("could not create extracted file {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, output_file);
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut written = 0u64;
    loop {
        let count = entry.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        written += count as u64;
        completed.fetch_add(count as u64, Ordering::Relaxed);
    }
    writer.flush()?;
    ensure!(
        written == meta.size,
        "ZIP entry has the wrong expanded size"
    );
    Ok(())
}

fn verify_zip_entry(
    archive: &mut zip::ZipArchive<File>,
    meta: &ZipEntryMeta,
    completed: &AtomicU64,
) -> Result<()> {
    if meta.is_dir {
        return Ok(());
    }
    let mut entry = archive.by_index(meta.index)?;
    ensure!(
        entry.enclosed_name().as_deref() == Some(meta.path.as_path()),
        "ZIP directory changed while verifying"
    );
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut read = 0u64;
    loop {
        let count = entry.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        completed.fetch_add(count as u64, Ordering::Relaxed);
    }
    ensure!(read == meta.size, "ZIP entry has the wrong expanded size");
    Ok(())
}

fn run_parallel_progress<T: Send>(
    phase: ProgressPhase,
    total: u64,
    progress: &mut impl FnMut(ProgressInfo),
    job: impl FnOnce(&AtomicU64) -> Result<T> + Send,
) -> Result<T> {
    let completed = AtomicU64::new(0);
    progress(ProgressInfo {
        phase,
        completed: 0,
        total,
    });
    thread::scope(|scope| {
        let completed_ref = &completed;
        let (sender, receiver) = sync_channel(1);
        let handle = scope.spawn(move || {
            let result = job(completed_ref);
            let _ = sender.send(result);
        });
        let result = loop {
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result,
                Err(RecvTimeoutError::Timeout) => progress(ProgressInfo {
                    phase,
                    completed: completed.load(Ordering::Relaxed).min(total),
                    total,
                }),
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(anyhow!("parallel worker stopped unexpectedly"));
                }
            }
        };
        handle
            .join()
            .map_err(|_| anyhow!("parallel worker panicked"))?;
        let result = result?;
        progress(ProgressInfo {
            phase,
            completed: total,
            total,
        });
        Ok(result)
    })
}

fn segmented_analysis(
    data: &[u8],
    target: usize,
    progress: &mut impl FnMut(ProgressInfo),
) -> Result<(Vec<usize>, AnalysisBackend, [u8; 32])> {
    if data.is_empty() {
        return Ok((
            vec![0],
            AnalysisBackend::Cpu,
            *blake3::hash(data).as_bytes(),
        ));
    }
    let mut boundaries = vec![0usize];
    let mut backend = AnalysisBackend::Cpu;
    let mut hasher = blake3::Hasher::new();
    for segment_start in (0..data.len()).step_by(ANALYSIS_SEGMENT) {
        let segment_end = (segment_start + ANALYSIS_SEGMENT).min(data.len());
        let segment = &data[segment_start..segment_end];
        hasher.update(segment);
        let report = analyze(segment, target, data.len())?;
        if matches!(backend, AnalysisBackend::Cpu)
            && let AnalysisBackend::Hybrid(_) = &report.backend
        {
            backend = report.backend.clone();
        }
        for local in report.boundaries.into_iter().skip(1) {
            let global = segment_start + local;
            if boundaries.last() != Some(&global) {
                boundaries.push(global);
            }
        }
        progress(ProgressInfo {
            phase: ProgressPhase::Analyzing,
            completed: segment_end as u64,
            total: data.len() as u64,
        });
    }
    if boundaries.last() != Some(&data.len()) {
        boundaries.push(data.len());
    }
    Ok((boundaries, backend, *hasher.finalize().as_bytes()))
}

fn encode_fast_chunk(offset: usize, source: &[u8], compression_level: i32) -> Result<EncodedChunk> {
    ensure!(source.len() <= u32::MAX as usize, "chunk is too large");
    let (compressed, compressed_codec) = if compression_level <= 0 {
        (lz4_flex::block::compress(source), 2)
    } else {
        (
            zstd::bulk::compress(source, compression_level).context("zstd compression failed")?,
            1,
        )
    };
    let (codec, payload) = if compressed.len() < source.len() {
        (compressed_codec, compressed)
    } else {
        (0, source.to_vec())
    };
    ensure!(
        payload.len() <= u32::MAX as usize,
        "stored chunk is too large"
    );
    Ok(EncodedChunk {
        offset: offset as u64,
        original_len: source.len() as u32,
        codec,
        checksum: *blake3::hash(source).as_bytes(),
        payload,
    })
}

fn decode_chunk(chunk: &ParsedChunk<'_>) -> Result<Vec<u8>> {
    let mut data = vec![0u8; chunk.original_len];
    decode_chunk_into(chunk, &mut data)?;
    Ok(data)
}

fn decode_chunk_into(chunk: &ParsedChunk<'_>, output: &mut [u8]) -> Result<()> {
    ensure!(
        output.len() == chunk.original_len,
        "chunk has the wrong length"
    );
    let written = match chunk.codec {
        0 => {
            ensure!(
                chunk.payload.len() == output.len(),
                "raw chunk has the wrong length"
            );
            output.copy_from_slice(chunk.payload);
            output.len()
        }
        1 => zstd::bulk::decompress_to_buffer(chunk.payload, output)
            .context("zstd decompression failed")?,
        2 => lz4_flex::block::decompress_into(chunk.payload, output)
            .context("LZ4 decompression failed")?,
        codec => bail!("unsupported chunk codec {codec}"),
    };
    ensure!(
        written == output.len(),
        "chunk has the wrong decoded length"
    );
    ensure!(
        blake3::hash(output).as_bytes() == &chunk.checksum,
        "checksum mismatch in chunk at offset {}",
        chunk.offset
    );
    Ok(())
}

fn write_header(
    writer: &mut impl Write,
    original_size: u64,
    chunk_count: usize,
    target_chunk_size: usize,
    whole_hash: [u8; 32],
) -> Result<()> {
    writer.write_all(MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&0u16.to_le_bytes())?;
    writer.write_all(&original_size.to_le_bytes())?;
    writer.write_all(&(chunk_count as u32).to_le_bytes())?;
    writer.write_all(&(target_chunk_size as u32).to_le_bytes())?;
    writer.write_all(&whole_hash)?;
    Ok(())
}

fn write_chunk(writer: &mut impl Write, chunk: &EncodedChunk) -> Result<()> {
    writer.write_all(&chunk.offset.to_le_bytes())?;
    writer.write_all(&chunk.original_len.to_le_bytes())?;
    writer.write_all(&(chunk.payload.len() as u32).to_le_bytes())?;
    writer.write_all(&[chunk.codec, 0, 0, 0])?;
    writer.write_all(&chunk.checksum)?;
    writer.write_all(&chunk.payload)?;
    Ok(())
}

fn map_non_empty(file: &File, len: usize) -> Result<Option<Mmap>> {
    if len == 0 {
        return Ok(None);
    }
    // SAFETY: the mapping is read-only and `file` is never mutated while it is alive.
    let mapping = unsafe { MmapOptions::new().map(file) }.context("could not memory-map input")?;
    Ok(Some(mapping))
}

fn map_required(file: &File) -> Result<Mmap> {
    // SAFETY: the mapping is read-only and `file` is never mutated while it is alive.
    unsafe { MmapOptions::new().map(file) }.context("could not memory-map archive")
}

fn ensure_distinct(input: &Path, output: &Path) -> Result<()> {
    ensure!(input != output, "input and output paths must be different");
    if input.exists() && output.exists() {
        ensure!(
            fs::canonicalize(input)? != fs::canonicalize(output)?,
            "input and output resolve to the same file"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn balanced_and_fast_file_modes_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.bin");
        let archive = temp.path().join("input.fst");
        let restored = temp.path().join("restored.bin");
        let data: Vec<u8> = (0..3_000_000).map(|index| (index % 37) as u8).collect();
        fs::write(&input, &data).unwrap();
        let stats = compress_file(
            &input,
            &archive,
            &CompressOptions {
                target_chunk_size: 256 * 1024,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(stats.archive_size < stats.original_size);
        assert!(matches!(
            stats.backend,
            AnalysisBackend::Cpu | AnalysisBackend::Hybrid(_)
        ));
        let archive_file = File::open(&archive).unwrap();
        let mapping = map_required(&archive_file).unwrap();
        let parsed = parse_archive(&mapping).unwrap();
        assert!(parsed.chunks.iter().all(|chunk| chunk.codec == 1));
        let mut progress = Vec::new();
        verify_file_with_progress(&archive, |info| progress.push(info)).unwrap();
        decompress_file_with_progress(&archive, &restored, |info| progress.push(info)).unwrap();
        assert!(progress.iter().any(|info| {
            info.phase == ProgressPhase::Decompressing && info.completed == info.total
        }));
        assert_eq!(fs::read(restored).unwrap(), data);

        let fast_archive = temp.path().join("input-fast.fst");
        let fast_stats = compress_file(
            &input,
            &fast_archive,
            &CompressOptions {
                compression_level: 0,
                target_chunk_size: 256 * 1024,
            },
        )
        .unwrap();
        assert!(matches!(
            fast_stats.backend,
            AnalysisBackend::Cpu | AnalysisBackend::Hybrid(_)
        ));
        let fast_file = File::open(&fast_archive).unwrap();
        let fast_mapping = map_required(&fast_file).unwrap();
        let fast_parsed = parse_archive(&fast_mapping).unwrap();
        assert!(fast_parsed.chunks.iter().all(|chunk| chunk.codec == 2));
    }

    #[test]
    fn empty_file_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("empty");
        let archive = temp.path().join("empty.fst");
        let restored = temp.path().join("empty.restored");
        fs::write(&input, []).unwrap();
        compress_file(&input, &archive, &CompressOptions::default()).unwrap();
        decompress_file(&archive, &restored).unwrap();
        assert_eq!(fs::metadata(restored).unwrap().len(), 0);
    }

    #[test]
    fn zip_extract_and_verify() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("sample.zip");
        let output = temp.path().join("extracted");
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("folder/hello.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"hello from zip").unwrap();
        zip.start_file("second.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&vec![42; 2 * 1024 * 1024]).unwrap();
        zip.finish().unwrap();

        let mut progress = Vec::new();
        let verified =
            verify_zip_file_with_progress(&archive_path, |info| progress.push(info)).unwrap();
        assert_eq!(verified.entries, 2);
        let extracted =
            extract_zip_file_with_progress(&archive_path, &output, |info| progress.push(info))
                .unwrap();
        assert_eq!(extracted.entries, 2);
        assert!(progress.iter().any(|info| {
            info.phase == ProgressPhase::ZipExtracting && info.completed == info.total
        }));
        assert_eq!(
            fs::read(output.join("folder/hello.txt")).unwrap(),
            b"hello from zip"
        );
        assert_eq!(
            fs::metadata(output.join("second.txt")).unwrap().len(),
            2 * 1024 * 1024
        );
    }

    #[test]
    fn zip_compress_streaming_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.bin");
        let archive = temp.path().join("input.zip");
        let output = temp.path().join("output");
        let data: Vec<u8> = (0..3_000_000).map(|index| (index % 193) as u8).collect();
        fs::write(&input, &data).unwrap();

        let mut progress = Vec::new();
        let report =
            compress_zip_file_with_progress(&input, &archive, 1, |info| progress.push(info))
                .unwrap();
        assert_eq!(report.original_size, data.len() as u64);
        assert_eq!(report.entries, 1);
        assert!(report.archive_size > 0);
        assert!(progress.iter().any(|info| {
            info.phase == ProgressPhase::ZipCompressing && info.completed == info.total
        }));

        extract_zip_file(&archive, &output).unwrap();
        assert_eq!(fs::read(output.join("input.bin")).unwrap(), data);
    }
}
