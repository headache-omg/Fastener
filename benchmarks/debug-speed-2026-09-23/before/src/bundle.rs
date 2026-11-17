use crate::{
    CompressOptions, ProgressInfo, compress_file_with_progress, decompress_file_with_progress,
    verify_file_with_progress,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    path::{Component, Path, PathBuf},
};

pub const DIRECTORY_MAGIC: &[u8; 8] = b"FASTDIR1";
const DIRECTORY_VERSION: u16 = 1;
const KIND_FILE: u8 = 0;
const KIND_DIRECTORY: u8 = 1;

#[derive(Clone, Debug)]
pub struct DirectoryReport {
    pub entries: usize,
    pub files: usize,
    pub original_size: u64,
    pub archive_size: u64,
    pub output: Option<PathBuf>,
    pub worker_threads: usize,
}

#[derive(Clone, Debug)]
struct SourceEntry {
    relative: PathBuf,
    source: PathBuf,
    kind: u8,
    size: u64,
}

#[derive(Clone, Debug)]
struct BundleEntry {
    relative: PathBuf,
    kind: u8,
    original_size: u64,
    archive_size: u64,
}

pub fn compress_directory_bundle_with_progress(
    input: &Path,
    output: &Path,
    options: &CompressOptions,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<DirectoryReport> {
    ensure!(input.is_dir(), "input is not a directory");
    ensure!(!output.exists(), "output archive already exists");
    let entries = collect_entries(input)?;
    let files = entries
        .iter()
        .filter(|entry| entry.kind == KIND_FILE)
        .count();
    let original_size = entries
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.size));
    ensure!(
        entries.len() <= u32::MAX as usize,
        "too many directory entries"
    );
    ensure!(files <= u32::MAX as usize, "too many files");

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".fastener-bundle-")
        .tempdir_in(parent)
        .context("could not create bundle workspace")?;
    let output_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .with_context(|| format!("could not create directory archive {}", output.display()))?;

    let result = (|| -> Result<u64> {
        let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, output_file);
        write_bundle_header(
            &mut writer,
            entries.len() as u32,
            files as u32,
            original_size,
        )?;
        let mut completed_before = 0u64;
        for (index, entry) in entries.iter().enumerate() {
            let encoded_path = encode_relative_path(&entry.relative)?;
            if entry.kind == KIND_DIRECTORY {
                write_entry_header(&mut writer, KIND_DIRECTORY, &encoded_path, 0, 0)?;
                continue;
            }

            let temporary_archive = temporary.path().join(format!("entry-{index}.fst"));
            let base = completed_before;
            compress_file_with_progress(&entry.source, &temporary_archive, options, |info| {
                progress(ProgressInfo {
                    phase: info.phase,
                    completed: base.saturating_add(info.completed).min(original_size),
                    total: original_size,
                });
            })?;
            let archive_size = fs::metadata(&temporary_archive)?.len();
            write_entry_header(
                &mut writer,
                KIND_FILE,
                &encoded_path,
                entry.size,
                archive_size,
            )?;
            let mut archive_reader =
                BufReader::with_capacity(8 * 1024 * 1024, File::open(&temporary_archive)?);
            let copied = io::copy(&mut archive_reader, &mut writer)?;
            ensure!(copied == archive_size, "temporary FST size changed");
            fs::remove_file(&temporary_archive)?;
            completed_before = completed_before.saturating_add(entry.size);
        }
        writer.flush()?;
        Ok(writer.stream_position()?)
    })();

    match result {
        Ok(archive_size) => Ok(DirectoryReport {
            entries: entries.len(),
            files,
            original_size,
            archive_size,
            output: Some(output.to_owned()),
            worker_threads: rayon::current_num_threads().min(files),
        }),
        Err(error) => {
            let _ = fs::remove_file(output);
            Err(error)
        }
    }
}

pub fn decompress_directory_bundle_with_progress(
    input: &Path,
    output_directory: &Path,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<DirectoryReport> {
    ensure!(
        !output_directory.exists(),
        "directory output already exists: {}",
        output_directory.display()
    );
    let input_file = File::open(input)
        .with_context(|| format!("could not open directory archive {}", input.display()))?;
    let archive_size = input_file.metadata()?.len();
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, input_file);
    let (entry_count, file_count, original_size) = read_bundle_header(&mut reader)?;
    let parent = output_directory.parent().unwrap_or_else(|| Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".fastener-extract-")
        .tempdir_in(parent)
        .context("could not create extraction workspace")?;
    fs::create_dir_all(output_directory)?;
    let mut seen = HashSet::with_capacity(entry_count);
    let mut completed_before = 0u64;
    let mut observed_files = 0usize;
    let mut observed_size = 0u64;
    let mut worker_threads = 0usize;

    for index in 0..entry_count {
        let entry = read_entry_header(&mut reader)?;
        ensure_unique_path(&entry.relative, &mut seen)?;
        let destination = output_directory.join(&entry.relative);
        if entry.kind == KIND_DIRECTORY {
            fs::create_dir_all(&destination)?;
            continue;
        }
        ensure!(entry.kind == KIND_FILE, "unknown directory entry kind");
        observed_files += 1;
        observed_size = observed_size.saturating_add(entry.original_size);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary_archive = temporary.path().join(format!("entry-{index}.fst"));
        copy_embedded_archive(&mut reader, &temporary_archive, entry.archive_size)?;
        let base = completed_before;
        let report = decompress_file_with_progress(&temporary_archive, &destination, |info| {
            progress(ProgressInfo {
                phase: info.phase,
                completed: base.saturating_add(info.completed).min(original_size),
                total: original_size,
            });
        })?;
        ensure!(
            report.original_size == entry.original_size,
            "embedded FST size does not match directory index"
        );
        worker_threads = worker_threads.max(report.worker_threads);
        fs::remove_file(&temporary_archive)?;
        completed_before = completed_before.saturating_add(entry.original_size);
    }
    ensure!(
        observed_files == file_count,
        "directory file count mismatch"
    );
    ensure!(
        observed_size == original_size,
        "directory expanded size mismatch"
    );
    ensure_reader_finished(&mut reader)?;

    Ok(DirectoryReport {
        entries: entry_count,
        files: file_count,
        original_size,
        archive_size,
        output: Some(output_directory.to_owned()),
        worker_threads,
    })
}

pub fn verify_directory_bundle_with_progress(
    input: &Path,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<DirectoryReport> {
    let input_file = File::open(input)
        .with_context(|| format!("could not open directory archive {}", input.display()))?;
    let archive_size = input_file.metadata()?.len();
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, input_file);
    let (entry_count, file_count, original_size) = read_bundle_header(&mut reader)?;
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".fastener-verify-")
        .tempdir_in(parent)
        .context("could not create verification workspace")?;
    let mut seen = HashSet::with_capacity(entry_count);
    let mut completed_before = 0u64;
    let mut observed_files = 0usize;
    let mut observed_size = 0u64;
    let mut worker_threads = 0usize;

    for index in 0..entry_count {
        let entry = read_entry_header(&mut reader)?;
        ensure_unique_path(&entry.relative, &mut seen)?;
        if entry.kind == KIND_DIRECTORY {
            continue;
        }
        ensure!(entry.kind == KIND_FILE, "unknown directory entry kind");
        observed_files += 1;
        observed_size = observed_size.saturating_add(entry.original_size);
        let temporary_archive = temporary.path().join(format!("entry-{index}.fst"));
        copy_embedded_archive(&mut reader, &temporary_archive, entry.archive_size)?;
        let base = completed_before;
        let report = verify_file_with_progress(&temporary_archive, |info| {
            progress(ProgressInfo {
                phase: info.phase,
                completed: base.saturating_add(info.completed).min(original_size),
                total: original_size,
            });
        })?;
        ensure!(
            report.original_size == entry.original_size,
            "embedded FST size does not match directory index"
        );
        worker_threads = worker_threads.max(report.worker_threads);
        fs::remove_file(&temporary_archive)?;
        completed_before = completed_before.saturating_add(entry.original_size);
    }
    ensure!(
        observed_files == file_count,
        "directory file count mismatch"
    );
    ensure!(
        observed_size == original_size,
        "directory expanded size mismatch"
    );
    ensure_reader_finished(&mut reader)?;

    Ok(DirectoryReport {
        entries: entry_count,
        files: file_count,
        original_size,
        archive_size,
        output: None,
        worker_threads,
    })
}

fn collect_entries(root: &Path) -> Result<Vec<SourceEntry>> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("could not read directory {}", directory.display()))?
        {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let source = entry.path();
            let relative = source.strip_prefix(root)?.to_owned();
            if file_type.is_dir() {
                entries.push(SourceEntry {
                    relative,
                    source: source.clone(),
                    kind: KIND_DIRECTORY,
                    size: 0,
                });
                pending.push(source);
            } else if file_type.is_file() {
                let size = entry.metadata()?.len();
                entries.push(SourceEntry {
                    relative,
                    source,
                    kind: KIND_FILE,
                    size,
                });
            }
        }
    }
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(entries)
}

fn encode_relative_path(path: &Path) -> Result<Vec<u8>> {
    validate_relative_path(path)?;
    let encoded = path.to_string_lossy().replace('\\', "/").into_bytes();
    ensure!(!encoded.is_empty(), "empty directory entry path");
    ensure!(
        encoded.len() <= u32::MAX as usize,
        "directory path is too long"
    );
    Ok(encoded)
}

fn decode_relative_path(encoded: Vec<u8>) -> Result<PathBuf> {
    let text = String::from_utf8(encoded).context("directory path is not UTF-8")?;
    let path = PathBuf::from(text.replace('/', std::path::MAIN_SEPARATOR_STR));
    validate_relative_path(&path)?;
    Ok(path)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(!path.as_os_str().is_empty(), "empty directory entry path");
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "unsafe directory entry path"
    );
    Ok(())
}

fn ensure_unique_path(path: &Path, seen: &mut HashSet<String>) -> Result<()> {
    let key = path.to_string_lossy().replace('\\', "/").to_lowercase();
    ensure!(seen.insert(key), "duplicate directory entry path");
    Ok(())
}

fn write_bundle_header(
    writer: &mut impl Write,
    entries: u32,
    files: u32,
    original_size: u64,
) -> Result<()> {
    writer.write_all(DIRECTORY_MAGIC)?;
    writer.write_all(&DIRECTORY_VERSION.to_le_bytes())?;
    writer.write_all(&0u16.to_le_bytes())?;
    writer.write_all(&entries.to_le_bytes())?;
    writer.write_all(&files.to_le_bytes())?;
    writer.write_all(&original_size.to_le_bytes())?;
    Ok(())
}

fn read_bundle_header(reader: &mut impl Read) -> Result<(usize, usize, u64)> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    ensure!(
        &magic == DIRECTORY_MAGIC,
        "not a Fastener directory archive"
    );
    let version = read_u16(reader)?;
    ensure!(
        version == DIRECTORY_VERSION,
        "unsupported directory archive version"
    );
    let _flags = read_u16(reader)?;
    let entries = read_u32(reader)? as usize;
    let files = read_u32(reader)? as usize;
    let original_size = read_u64(reader)?;
    ensure!(files <= entries, "invalid directory archive counts");
    Ok((entries, files, original_size))
}

fn write_entry_header(
    writer: &mut impl Write,
    kind: u8,
    path: &[u8],
    original_size: u64,
    archive_size: u64,
) -> Result<()> {
    writer.write_all(&[kind, 0, 0, 0])?;
    writer.write_all(&(path.len() as u32).to_le_bytes())?;
    writer.write_all(&original_size.to_le_bytes())?;
    writer.write_all(&archive_size.to_le_bytes())?;
    writer.write_all(path)?;
    Ok(())
}

fn read_entry_header(reader: &mut impl Read) -> Result<BundleEntry> {
    let mut kind = [0u8; 4];
    reader.read_exact(&mut kind)?;
    ensure!(kind[1..] == [0, 0, 0], "invalid directory entry flags");
    let path_len = read_u32(reader)? as usize;
    ensure!(
        path_len > 0 && path_len <= 1024 * 1024,
        "invalid directory path length"
    );
    let original_size = read_u64(reader)?;
    let archive_size = read_u64(reader)?;
    let mut path = vec![0u8; path_len];
    reader.read_exact(&mut path)?;
    if kind[0] == KIND_DIRECTORY {
        ensure!(
            original_size == 0 && archive_size == 0,
            "invalid directory record"
        );
    }
    Ok(BundleEntry {
        relative: decode_relative_path(path)?,
        kind: kind[0],
        original_size,
        archive_size,
    })
}

fn copy_embedded_archive(reader: &mut impl Read, output: &Path, archive_size: u64) -> Result<()> {
    ensure!(archive_size > 0, "embedded FST is empty");
    let output_file = File::create(output)?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, output_file);
    let mut limited = reader.take(archive_size);
    let copied = io::copy(&mut limited, &mut writer)?;
    writer.flush()?;
    ensure!(copied == archive_size, "truncated embedded FST");
    Ok(())
}

fn ensure_reader_finished(reader: &mut impl Read) -> Result<()> {
    let mut trailing = [0u8; 1];
    ensure!(
        reader.read(&mut trailing)? == 0,
        "trailing directory archive data"
    );
    Ok(())
}

fn read_u16(reader: &mut impl Read) -> Result<u16> {
    let mut bytes = [0u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_bundle_round_trip_preserves_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let nested = source.join("nested");
        let empty = source.join("empty");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(&empty).unwrap();
        fs::write(source.join("one.txt"), b"one one one").unwrap();
        fs::write(nested.join("two.bin"), vec![42; 200_000]).unwrap();
        let archive = temporary.path().join("source.fst");
        let restored = temporary.path().join("restored");
        let options = CompressOptions::default();

        let compressed =
            compress_directory_bundle_with_progress(&source, &archive, &options, |_| {}).unwrap();
        assert_eq!(compressed.files, 2);
        verify_directory_bundle_with_progress(&archive, |_| {}).unwrap();
        let decompressed =
            decompress_directory_bundle_with_progress(&archive, &restored, |_| {}).unwrap();
        assert_eq!(decompressed.files, 2);
        assert_eq!(fs::read(restored.join("one.txt")).unwrap(), b"one one one");
        assert_eq!(
            fs::read(restored.join("nested/two.bin")).unwrap(),
            vec![42; 200_000]
        );
        assert!(restored.join("empty").is_dir());
    }
}
