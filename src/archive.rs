use crate::analyzer::{AnalysisBackend, analyze};
use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;

pub(crate) const MAGIC: &[u8; 8] = b"FASTENR1";
pub(crate) const VERSION: u16 = 1;
pub(crate) const HEADER_LEN: usize = 8 + 2 + 2 + 8 + 4 + 4 + 32;
pub(crate) const RECORD_LEN: usize = 8 + 4 + 4 + 1 + 3 + 32;
pub(crate) const MAX_CHUNKS: usize = 1_000_000;
/// The file encoder analyzes at most one 64 MiB segment at a time.
pub(crate) const MAX_DECODED_CHUNK: usize = 64 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Resource limit for reading ordinary FST files and directory bundles.
#[derive(Clone, Copy, Debug)]
pub struct FstLimits {
    pub max_output_bytes: u64,
}

impl Default for FstLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: MAX_EXPANDED_BYTES,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompressOptions {
    pub target_chunk_size: usize,
    pub compression_level: i32,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            target_chunk_size: 8 * 1024 * 1024,
            compression_level: 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ArchiveStats {
    pub original_size: u64,
    pub archive_size: u64,
    pub chunk_count: usize,
    pub backend: AnalysisBackend,
    pub raw_chunks: usize,
}

impl ArchiveStats {
    pub fn ratio(&self) -> f64 {
        if self.original_size == 0 {
            1.0
        } else {
            self.archive_size as f64 / self.original_size as f64
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub original_size: u64,
    pub archive_size: u64,
    pub chunk_count: usize,
    /// Rayon workers that participated in this operation when measured.
    pub worker_threads: usize,
}

struct EncodedChunk {
    offset: u64,
    original_len: u32,
    codec: u8,
    checksum: [u8; 32],
    payload: Vec<u8>,
}

pub(crate) struct ParsedArchive<'a> {
    pub(crate) original_size: usize,
    pub(crate) whole_hash: [u8; 32],
    pub(crate) chunks: Vec<ParsedChunk<'a>>,
}

pub(crate) struct ParsedChunk<'a> {
    pub(crate) offset: usize,
    pub(crate) original_len: usize,
    pub(crate) codec: u8,
    pub(crate) checksum: [u8; 32],
    pub(crate) payload: &'a [u8],
}

pub fn compress_bytes(data: &[u8], options: &CompressOptions) -> Result<(Vec<u8>, ArchiveStats)> {
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
    let analysis = analyze(data, options.target_chunk_size, data.len())?;
    ensure!(
        analysis
            .boundaries
            .windows(2)
            .all(|range| range[1] - range[0] <= MAX_DECODED_CHUNK),
        "chunk exceeds the 64 MiB format limit"
    );

    let chunks: Result<Vec<_>> = analysis
        .boundaries
        .par_windows(2)
        .map(|range| {
            let start = range[0];
            let end = range[1];
            let source = &data[start..end];
            let (compressed, compressed_codec) = if options.compression_level <= 0 {
                (lz4_flex::block::compress(source), 2)
            } else {
                (
                    zstd::bulk::compress(source, options.compression_level)
                        .context("zstd compression failed")?,
                    1,
                )
            };
            let (codec, payload) = if compressed.len() < source.len() {
                (compressed_codec, compressed)
            } else {
                (0, source.to_vec())
            };
            Ok(EncodedChunk {
                offset: start as u64,
                original_len: source.len() as u32,
                codec,
                checksum: *blake3::hash(source).as_bytes(),
                payload,
            })
        })
        .collect();
    let chunks = chunks?;
    ensure!(
        chunks.len() <= MAX_CHUNKS,
        "archive would contain too many chunks"
    );

    let archive_capacity = HEADER_LEN
        + chunks
            .iter()
            .map(|chunk| RECORD_LEN + chunk.payload.len())
            .sum::<usize>();
    let mut out = Vec::with_capacity(archive_capacity);
    out.extend_from_slice(MAGIC);
    push_u16(&mut out, VERSION);
    push_u16(&mut out, 0);
    push_u64(&mut out, data.len() as u64);
    push_u32(&mut out, chunks.len() as u32);
    push_u32(&mut out, options.target_chunk_size as u32);
    out.extend_from_slice(blake3::hash(data).as_bytes());
    for chunk in &chunks {
        push_u64(&mut out, chunk.offset);
        push_u32(&mut out, chunk.original_len);
        push_u32(&mut out, chunk.payload.len() as u32);
        out.push(chunk.codec);
        out.extend_from_slice(&[0; 3]);
        out.extend_from_slice(&chunk.checksum);
        out.extend_from_slice(&chunk.payload);
    }

    let stats = ArchiveStats {
        original_size: data.len() as u64,
        archive_size: out.len() as u64,
        chunk_count: chunks.len(),
        backend: analysis.backend,
        raw_chunks: chunks.iter().filter(|chunk| chunk.codec == 0).count(),
    };
    Ok((out, stats))
}

pub fn decompress_bytes(archive: &[u8]) -> Result<Vec<u8>> {
    decompress_bytes_with_limits(archive, FstLimits::default())
}

pub fn decompress_bytes_with_limits(archive: &[u8], limits: FstLimits) -> Result<Vec<u8>> {
    let parsed = parse_archive_with_limits(archive, limits)?;
    let decoded: Result<Vec<(usize, Vec<u8>)>> = parsed
        .chunks
        .par_iter()
        .map(|chunk| {
            let data = match chunk.codec {
                0 => chunk.payload.to_vec(),
                1 => zstd::bulk::decompress(chunk.payload, chunk.original_len)
                    .context("zstd decompression failed")?,
                2 => lz4_flex::block::decompress(chunk.payload, chunk.original_len)
                    .context("LZ4 decompression failed")?,
                codec => bail!("unsupported chunk codec {codec}"),
            };
            ensure!(
                data.len() == chunk.original_len,
                "chunk at offset {} has the wrong length",
                chunk.offset
            );
            ensure!(
                blake3::hash(&data).as_bytes() == &chunk.checksum,
                "checksum mismatch in chunk at offset {}",
                chunk.offset
            );
            Ok((chunk.offset, data))
        })
        .collect();

    let decoded = decoded?;
    let mut output = vec![0u8; parsed.original_size];
    for (offset, chunk) in decoded {
        output[offset..offset + chunk.len()].copy_from_slice(&chunk);
    }
    ensure!(
        blake3::hash(&output).as_bytes() == &parsed.whole_hash,
        "whole-file checksum mismatch"
    );
    Ok(output)
}

pub fn verify_bytes(archive: &[u8]) -> Result<VerifyReport> {
    verify_bytes_with_limits(archive, FstLimits::default())
}

pub fn verify_bytes_with_limits(archive: &[u8], limits: FstLimits) -> Result<VerifyReport> {
    let parsed = parse_archive_with_limits(archive, limits)?;
    let original_size = parsed.original_size as u64;
    let chunk_count = parsed.chunks.len();
    let _ = decompress_bytes_with_limits(archive, limits)?;
    Ok(VerifyReport {
        original_size,
        archive_size: archive.len() as u64,
        chunk_count,
        worker_threads: rayon::current_num_threads().min(chunk_count),
    })
}

pub fn inspect_archive(archive: &[u8]) -> Result<VerifyReport> {
    inspect_archive_with_limits(archive, FstLimits::default())
}

pub fn inspect_archive_with_limits(archive: &[u8], limits: FstLimits) -> Result<VerifyReport> {
    let parsed = parse_archive_with_limits(archive, limits)?;
    Ok(VerifyReport {
        original_size: parsed.original_size as u64,
        archive_size: archive.len() as u64,
        chunk_count: parsed.chunks.len(),
        worker_threads: 0,
    })
}

#[cfg(test)]
pub(crate) fn parse_archive(archive: &[u8]) -> Result<ParsedArchive<'_>> {
    parse_archive_with_limits(archive, FstLimits::default())
}

pub(crate) fn parse_archive_with_limits(
    archive: &[u8],
    limits: FstLimits,
) -> Result<ParsedArchive<'_>> {
    ensure!(
        archive.len() >= HEADER_LEN,
        "file is too short to be a Fastener archive"
    );
    ensure!(&archive[..8] == MAGIC, "invalid Fastener magic bytes");
    let mut cursor = 8;
    let version = take_u16(archive, &mut cursor)?;
    ensure!(version == VERSION, "unsupported Fastener version {version}");
    let flags = take_u16(archive, &mut cursor)?;
    ensure!(flags == 0, "unsupported Fastener archive flags");
    let original_size_u64 = take_u64(archive, &mut cursor)?;
    ensure!(
        original_size_u64 <= limits.max_output_bytes,
        "FST expanded size exceeds configured limit"
    );
    let original_size =
        usize::try_from(original_size_u64).context("original size does not fit this platform")?;
    let chunk_count = take_u32(archive, &mut cursor)? as usize;
    ensure!(
        chunk_count <= MAX_CHUNKS,
        "unreasonable chunk count {chunk_count}"
    );
    let _target_chunk_size = take_u32(archive, &mut cursor)?;
    let whole_hash = take_array::<32>(archive, &mut cursor)?;
    ensure!(
        chunk_count <= (archive.len() - HEADER_LEN) / RECORD_LEN,
        "truncated chunk records"
    );
    let mut chunks = Vec::with_capacity(chunk_count);
    let mut expected_offset = 0usize;

    for _ in 0..chunk_count {
        let offset = usize::try_from(take_u64(archive, &mut cursor)?)
            .context("chunk offset does not fit this platform")?;
        let original_len = take_u32(archive, &mut cursor)? as usize;
        ensure!(
            original_len <= MAX_DECODED_CHUNK,
            "decoded chunk exceeds the 64 MiB limit"
        );
        let stored_len = take_u32(archive, &mut cursor)? as usize;
        let codec = take_u8(archive, &mut cursor)?;
        ensure!(codec <= 2, "unsupported chunk codec {codec}");
        ensure!(original_len > 0, "empty chunk is not allowed");
        ensure!(
            codec != 0 || stored_len == original_len,
            "raw chunk has the wrong length"
        );
        let reserved_end = cursor.checked_add(3).context("archive offset overflow")?;
        ensure!(
            archive.get(cursor..reserved_end) == Some(&[0, 0, 0][..]),
            "invalid chunk flags or truncated record"
        );
        cursor = reserved_end;
        let checksum = take_array::<32>(archive, &mut cursor)?;
        ensure!(
            offset == expected_offset,
            "chunks are missing, overlapping, or out of order"
        );
        let end = cursor
            .checked_add(stored_len)
            .context("stored chunk length overflow")?;
        ensure!(end <= archive.len(), "truncated chunk payload");
        let original_end = offset
            .checked_add(original_len)
            .context("original chunk length overflow")?;
        ensure!(
            original_end <= original_size,
            "chunk exceeds declared original size"
        );
        chunks.push(ParsedChunk {
            offset,
            original_len,
            codec,
            checksum,
            payload: &archive[cursor..end],
        });
        expected_offset = original_end;
        cursor = end;
    }
    ensure!(
        expected_offset == original_size,
        "chunks do not cover the declared original size"
    );
    ensure!(cursor == archive.len(), "unexpected trailing data");
    if original_size == 0 {
        ensure!(chunk_count == 0, "empty archive must not contain chunks");
    }
    Ok(ParsedArchive {
        original_size,
        whole_hash,
        chunks,
    })
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn take_u8(data: &[u8], cursor: &mut usize) -> Result<u8> {
    let value = *data.get(*cursor).context("unexpected end of archive")?;
    *cursor += 1;
    Ok(value)
}
fn take_u16(data: &[u8], cursor: &mut usize) -> Result<u16> {
    Ok(u16::from_le_bytes(take_array(data, cursor)?))
}
fn take_u32(data: &[u8], cursor: &mut usize) -> Result<u32> {
    Ok(u32::from_le_bytes(take_array(data, cursor)?))
}
fn take_u64(data: &[u8], cursor: &mut usize) -> Result<u64> {
    Ok(u64::from_le_bytes(take_array(data, cursor)?))
}
fn take_array<const N: usize>(data: &[u8], cursor: &mut usize) -> Result<[u8; N]> {
    let end = cursor.checked_add(N).context("archive offset overflow")?;
    let bytes = data
        .get(*cursor..end)
        .context("unexpected end of archive")?;
    *cursor = end;
    Ok(bytes.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut data = Vec::new();
        for index in 0..30_000 {
            data.extend_from_slice(
                format!("record={index:06},kind=fastener,value={}\n", index % 17).as_bytes(),
            );
        }
        data.extend((0..300_000).map(|i| (i * 31) as u8));
        data
    }

    #[test]
    fn round_trip_parallel_archive() {
        let source = sample();
        let options = CompressOptions {
            target_chunk_size: 64 * 1024,
            ..Default::default()
        };
        let (archive, stats) = compress_bytes(&source, &options).unwrap();
        assert!(stats.chunk_count > 1);
        assert_eq!(decompress_bytes(&archive).unwrap(), source);
        assert_eq!(
            verify_bytes(&archive).unwrap().chunk_count,
            stats.chunk_count
        );
    }

    #[test]
    fn empty_input_round_trips() {
        let (archive, stats) = compress_bytes(&[], &CompressOptions::default()).unwrap();
        assert_eq!(stats.chunk_count, 0);
        assert_eq!(decompress_bytes(&archive).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn corruption_is_detected() {
        let source = sample();
        let (mut archive, _) = compress_bytes(&source, &CompressOptions::default()).unwrap();
        let last = archive.len() - 1;
        archive[last] ^= 0x40;
        assert!(verify_bytes(&archive).is_err());
    }

    #[test]
    fn malformed_archive_is_rejected() {
        assert!(decompress_bytes(b"not an archive").is_err());
        let (mut archive, _) = compress_bytes(b"contents", &CompressOptions::default()).unwrap();
        archive[10] = 1;
        assert!(inspect_archive(&archive).is_err());
        archive[10] = 0;
        archive[77] = 1;
        assert!(inspect_archive(&archive).is_err());
    }
}
