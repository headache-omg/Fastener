//! Versioned, authenticated chunk archives. See docs/暗号化仕様.md.
use crate::{
    CompressOptions, ProgressInfo, ProgressPhase, analyzer::analyze, path_safety::safe_path,
};
use anyhow::{Context, Result, bail, ensure};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{AeadInPlace, KeyInit},
};
use rayon::prelude::*;
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Cursor, Read, Write},
    path::{Component, Path},
};
use zeroize::Zeroizing;

pub const ENCRYPTED_MAGIC: &[u8; 8] = b"FSTENC01";
const HEADER_LEN: usize = 48;
const SEGMENT: usize = 64 * 1024 * 1024;
const MAX_CHUNK: usize = 16 * 1024 * 1024;
const MAX_FRAME: usize = MAX_CHUNK + 64;
const MAX_MANIFEST: usize = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_EXPANDED_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const FRAME_BATCH: usize = 8;
const AUTH_ERROR: &str =
    "認証失敗: パスワードが違うか、書庫が破損・改変されています (authentication failed)";

#[derive(Debug)]
pub struct EncryptedReport {
    pub original_size: u64,
    pub archive_size: u64,
    pub files: usize,
    pub chunks: usize,
    pub is_directory: bool,
}

/// Resource limit for an authenticated but potentially untrusted archive.
/// Raise this explicitly when reading a larger trusted archive.
#[derive(Clone, Copy, Debug)]
pub struct EncryptedLimits {
    pub max_output_bytes: u64,
}

impl Default for EncryptedLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: MAX_EXPANDED_BYTES,
        }
    }
}

#[derive(Debug)]
struct Entry {
    path: String,
    directory: bool,
    size: u64,
}

struct Crypto {
    header: [u8; HEADER_LEN],
    cipher: XChaCha20Poly1305,
}

impl Crypto {
    fn derive(header: [u8; HEADER_LEN], password: &[u8]) -> Result<Self> {
        validate_header(&header)?;
        ensure!(
            !password.is_empty() && password.len() <= 1024,
            "パスワードは1～1024バイトで指定してください"
        );
        let params = Params::new(64 * 1024, 3, 1, Some(32))
            .map_err(|_| anyhow::anyhow!("invalid internal KDF parameters"))?;
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(password, &header[16..32], key.as_mut())
            .map_err(|_| anyhow::anyhow!("password key derivation failed"))?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid internal key length"))?;
        Ok(Self { header, cipher })
    }

    fn nonce(&self, sequence: u64) -> [u8; 24] {
        let mut nonce = [0u8; 24];
        nonce[..16].copy_from_slice(&self.header[32..48]);
        nonce[16..].copy_from_slice(&sequence.to_le_bytes());
        nonce
    }

    fn aad(&self, sequence: u64, stored_len: usize) -> [u8; 60] {
        let mut aad = [0u8; 60];
        aad[..48].copy_from_slice(&self.header);
        aad[48..56].copy_from_slice(&sequence.to_le_bytes());
        aad[56..].copy_from_slice(&(stored_len as u32).to_le_bytes());
        aad
    }

    fn seal(&self, sequence: u64, mut plaintext: Zeroizing<Vec<u8>>) -> Result<Vec<u8>> {
        ensure!(
            plaintext.len() + 16 <= MAX_FRAME,
            "encrypted record is too large"
        );
        let aad = self.aad(sequence, plaintext.len() + 16);
        self.cipher
            .encrypt_in_place(
                XNonce::from_slice(&self.nonce(sequence)),
                &aad,
                &mut *plaintext,
            )
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        // This copy contains ciphertext only. The owned working buffer is wiped.
        Ok(plaintext.to_vec())
    }

    fn open(&self, sequence: u64, ciphertext: Vec<u8>) -> Result<Zeroizing<Vec<u8>>> {
        let aad = self.aad(sequence, ciphertext.len());
        let mut bytes = Zeroizing::new(ciphertext);
        self.cipher
            .decrypt_in_place(XNonce::from_slice(&self.nonce(sequence)), &aad, &mut *bytes)
            .map_err(|_| anyhow::anyhow!(AUTH_ERROR))?;
        Ok(bytes)
    }
}

fn validate_header(header: &[u8; HEADER_LEN]) -> Result<()> {
    ensure!(
        &header[..8] == ENCRYPTED_MAGIC,
        "not an encrypted Fastener archive"
    );
    ensure!(
        header[8..10] == [1, 0] && header[10] <= 1 && header[11] == 1,
        "unsupported encrypted archive version or KDF profile"
    );
    let target = u32::from_le_bytes(header[12..16].try_into()?) as usize;
    ensure!(
        (8192..=MAX_CHUNK / 2).contains(&target),
        "invalid encrypted chunk size"
    );
    Ok(())
}

fn read_header(reader: &mut impl Read) -> Result<[u8; HEADER_LEN]> {
    let mut header = [0u8; HEADER_LEN];
    reader
        .read_exact(&mut header)
        .context("truncated encrypted header")?;
    validate_header(&header)?;
    Ok(header)
}

/// A routing hint only. The header is authenticated when the password is used.
pub fn encrypted_is_directory(input: &Path) -> Result<bool> {
    Ok(read_header(&mut File::open(input)?)?[10] == 1)
}

fn entry_path(path: &Path) -> Result<String> {
    let parts = path
        .components()
        .map(|c| match c {
            Component::Normal(part) => part.to_str().context("entry name must be UTF-8"),
            _ => bail!("unsafe source path"),
        })
        .collect::<Result<Vec<_>>>()?;
    let path = parts.join("/");
    safe_path(&path)?;
    Ok(path)
}

fn collect_entries(input: &Path, directory: bool) -> Result<Vec<Entry>> {
    if !directory {
        ensure!(input.is_file(), "input must be a regular file or directory");
        return Ok(vec![Entry {
            path: entry_path(Path::new(input.file_name().context("missing file name")?))?,
            directory: false,
            size: fs::metadata(input)?.len(),
        }]);
    }
    let mut entries = Vec::new();
    let mut pending = vec![input.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for child in fs::read_dir(folder)? {
            let child = child?;
            let kind = child.file_type()?;
            if kind.is_symlink() || !(kind.is_file() || kind.is_dir()) {
                continue;
            }
            let path = child.path();
            entries.push(Entry {
                path: entry_path(path.strip_prefix(input)?)?,
                directory: kind.is_dir(),
                size: if kind.is_dir() {
                    0
                } else {
                    child.metadata()?.len()
                },
            });
            ensure!(entries.len() <= MAX_ENTRIES, "too many encrypted entries");
            if kind.is_dir() {
                pending.push(path);
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn validate_entries(entries: &[Entry], directory: bool) -> Result<u64> {
    ensure!(
        directory || (entries.len() == 1 && !entries[0].directory),
        "invalid single-file manifest"
    );
    let mut paths = HashMap::new();
    let mut total = 0u64;
    for entry in entries {
        safe_path(&entry.path)?;
        ensure!(
            !entry.directory || entry.size == 0,
            "directory entry has data"
        );
        ensure!(
            paths
                .insert(entry.path.to_lowercase(), entry.directory)
                .is_none(),
            "duplicate encrypted entry"
        );
        total = total
            .checked_add(entry.size)
            .context("manifest size overflow")?;
    }
    for entry in entries {
        let mut path = entry.path.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            ensure!(
                paths.get(&parent.to_lowercase()) == Some(&true),
                "missing directory or file/path collision"
            );
            path = parent;
        }
    }
    Ok(total)
}

fn encode_manifest(entries: &[Entry]) -> Result<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(vec![b'M']);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        out.push(u8::from(entry.directory));
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&(entry.path.len() as u32).to_le_bytes());
        out.extend_from_slice(entry.path.as_bytes());
        ensure!(
            out.len() <= MAX_MANIFEST,
            "encrypted manifest exceeds 8 MiB"
        );
    }
    Ok(out)
}

fn read_array<const N: usize>(reader: &mut impl Read) -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn decode_manifest(bytes: &[u8], directory: bool) -> Result<(Vec<Entry>, u64)> {
    ensure!(
        bytes.len() <= MAX_MANIFEST && bytes.first() == Some(&b'M'),
        "invalid encrypted manifest"
    );
    let mut reader = Cursor::new(&bytes[1..]);
    let count = u32::from_le_bytes(read_array(&mut reader)?) as usize;
    ensure!(
        count <= MAX_ENTRIES && count <= bytes.len() / 14,
        "unreasonable manifest count"
    );
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = read_array::<1>(&mut reader)?[0];
        ensure!(kind <= 1, "invalid entry kind");
        let size = u64::from_le_bytes(read_array(&mut reader)?);
        let len = u32::from_le_bytes(read_array(&mut reader)?) as usize;
        ensure!(len > 0 && len <= 4096, "invalid entry path length");
        let mut path = vec![0u8; len];
        reader.read_exact(&mut path)?;
        entries.push(Entry {
            path: String::from_utf8(path)?,
            directory: kind == 1,
            size,
        });
    }
    ensure!(
        reader.position() as usize == bytes.len() - 1,
        "trailing manifest data"
    );
    let size = validate_entries(&entries, directory)?;
    Ok((entries, size))
}

fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(bytes)?;
    Ok(())
}

fn read_frame(reader: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    if reader.read(&mut length[..1])? == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut length[1..])
        .context("truncated encrypted frame length")?;
    let len = u32::from_le_bytes(length) as usize;
    ensure!(
        (17..=MAX_FRAME).contains(&len),
        "invalid encrypted frame length"
    );
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .context("truncated encrypted frame")?;
    Ok(Some(bytes))
}

fn encode_data(data: &[u8], level: i32) -> Result<Zeroizing<Vec<u8>>> {
    let packed = Zeroizing::new(if level == 0 {
        lz4_flex::block::compress(data)
    } else {
        zstd::bulk::compress(data, level)?
    });
    let (codec, payload) = if packed.len() < data.len() {
        (if level == 0 { 2 } else { 1 }, packed.as_slice())
    } else {
        (0, data)
    };
    let mut out = Zeroizing::new(Vec::with_capacity(6 + payload.len()));
    out.push(b'D');
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.push(codec);
    out.extend_from_slice(payload);
    Ok(out)
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn distinct(input: &Path, output: &Path) -> Result<()> {
    ensure!(input != output, "input and output must differ");
    if output.exists() {
        ensure!(
            fs::canonicalize(input)? != fs::canonicalize(output)?,
            "input and output refer to the same path"
        );
    }
    Ok(())
}

fn next_sequence(sequence: &mut u64) -> Result<()> {
    *sequence = sequence
        .checked_add(1)
        .context("encrypted sequence exhausted")?;
    Ok(())
}

/// Compress and encrypt directly; no plaintext archive is written to disk.
pub fn compress_encrypted_with_progress(
    input: &Path,
    output: &Path,
    options: &CompressOptions,
    password: &[u8],
    mut progress: impl FnMut(ProgressInfo),
) -> Result<EncryptedReport> {
    ensure!(
        (8192..=MAX_CHUNK / 2).contains(&options.target_chunk_size),
        "暗号化時のチャンクサイズは8 KiB～8 MiBです"
    );
    ensure!(
        (0..=22).contains(&options.compression_level),
        "invalid compression level"
    );
    distinct(input, output)?;
    let directory = input.is_dir();
    fs::create_dir_all(parent(output))?;
    if directory {
        ensure!(
            !fs::canonicalize(parent(output))?.starts_with(fs::canonicalize(input)?),
            "書庫の出力先は入力フォルダーの外にしてください"
        );
    }
    let entries = collect_entries(input, directory)?;
    let total = validate_entries(&entries, directory)?;
    let mut header = [0u8; HEADER_LEN];
    header[..8].copy_from_slice(ENCRYPTED_MAGIC);
    header[8] = 1;
    header[10] = u8::from(directory);
    header[11] = 1;
    header[12..16].copy_from_slice(&(options.target_chunk_size as u32).to_le_bytes());
    getrandom::fill(&mut header[16..])
        .map_err(|_| anyhow::anyhow!("OS random generator failed"))?;
    progress(ProgressInfo {
        phase: ProgressPhase::Compressing,
        completed: 0,
        total,
    });
    let crypto = Crypto::derive(header, password)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".fastener-encrypted-")
        .tempfile_in(parent(output))?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, temporary.as_file_mut());
    writer.write_all(&header)?;
    write_frame(&mut writer, &crypto.seal(0, encode_manifest(&entries)?)?)?;
    let mut sequence = 1u64;
    let mut completed = 0u64;
    let mut chunks = 0usize;
    for entry in entries.iter().filter(|e| !e.directory) {
        let path = if directory {
            input.join(&entry.path)
        } else {
            input.to_path_buf()
        };
        let mut reader = File::open(path)?;
        ensure!(
            reader.metadata()?.len() == entry.size,
            "input changed during compression"
        );
        let mut remaining = entry.size;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = Zeroizing::new(vec![0u8; entry.size.min(SEGMENT as u64) as usize]);
        while remaining > 0 {
            let size = remaining.min(buffer.len() as u64) as usize;
            let data = &mut buffer[..size];
            reader
                .read_exact(data)
                .context("input shortened during compression")?;
            hasher.update_rayon(data);
            let analysis = analyze(
                data,
                options.target_chunk_size,
                usize::try_from(entry.size).unwrap_or(usize::MAX),
            )?;
            let ranges: Vec<_> = analysis.boundaries.windows(2).collect();
            for batch in ranges.chunks(FRAME_BATCH) {
                ensure!(
                    sequence.checked_add(batch.len() as u64).is_some(),
                    "encrypted sequence exhausted"
                );
                let encrypted = batch
                    .par_iter()
                    .enumerate()
                    .map(|(i, range)| {
                        crypto.seal(
                            sequence + i as u64,
                            encode_data(&data[range[0]..range[1]], options.compression_level)?,
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                for bytes in encrypted {
                    write_frame(&mut writer, &bytes)?;
                    next_sequence(&mut sequence)?;
                    chunks += 1;
                }
                completed += batch.iter().map(|r| (r[1] - r[0]) as u64).sum::<u64>();
                progress(ProgressInfo {
                    phase: ProgressPhase::Compressing,
                    completed,
                    total,
                });
            }
            remaining -= size as u64;
        }
        ensure!(
            reader.read(&mut [0u8; 1])? == 0,
            "input grew during compression"
        );
        let mut hash = Zeroizing::new(vec![b'H']);
        hash.extend_from_slice(hasher.finalize().as_bytes());
        write_frame(&mut writer, &crypto.seal(sequence, hash)?)?;
        next_sequence(&mut sequence)?;
    }
    write_frame(
        &mut writer,
        &crypto.seal(sequence, Zeroizing::new(vec![b'E']))?,
    )?;
    writer.flush()?;
    drop(writer);
    let archive_size = temporary.as_file().metadata()?.len();
    temporary
        .persist(output)
        .context("could not publish encrypted archive")?;
    progress(ProgressInfo {
        phase: ProgressPhase::Compressing,
        completed: total,
        total,
    });
    Ok(EncryptedReport {
        original_size: total,
        archive_size,
        files: entries.iter().filter(|e| !e.directory).count(),
        chunks,
        is_directory: directory,
    })
}

enum Record {
    Data(Zeroizing<Vec<u8>>),
    Hash([u8; 32]),
    End,
}

fn decode_record(mut bytes: Zeroizing<Vec<u8>>) -> Result<Record> {
    match bytes.first() {
        Some(b'D') => {
            ensure!(bytes.len() >= 6, "truncated data record");
            let size = u32::from_le_bytes(bytes[1..5].try_into()?) as usize;
            ensure!(size > 0 && size <= MAX_CHUNK, "invalid decoded chunk size");
            let data = match bytes[5] {
                0 => {
                    ensure!(bytes.len() - 6 == size, "invalid raw chunk size");
                    bytes.drain(..6);
                    bytes
                }
                1 => Zeroizing::new(zstd::bulk::decompress(&bytes[6..], size)?),
                2 => Zeroizing::new(lz4_flex::block::decompress(&bytes[6..], size)?),
                _ => bail!("unsupported encrypted chunk codec"),
            };
            ensure!(data.len() == size, "decoded chunk length mismatch");
            Ok(Record::Data(data))
        }
        Some(b'H') if bytes.len() == 33 => Ok(Record::Hash(bytes[1..].try_into()?)),
        Some(b'E') if bytes.len() == 1 => Ok(Record::End),
        _ => bail!("invalid encrypted record type"),
    }
}

struct Records<'a, R> {
    reader: R,
    crypto: &'a Crypto,
    sequence: u64,
    pending: VecDeque<Record>,
}

impl<R: Read> Records<'_, R> {
    fn next(&mut self) -> Result<Record> {
        if self.pending.is_empty() {
            let mut batch = Vec::new();
            for _ in 0..FRAME_BATCH {
                let Some(bytes) = read_frame(&mut self.reader)? else {
                    break;
                };
                batch.push((self.sequence, bytes));
                next_sequence(&mut self.sequence)?;
            }
            self.pending = batch
                .into_par_iter()
                .map(|(seq, bytes)| decode_record(self.crypto.open(seq, bytes)?))
                .collect::<Result<VecDeque<_>>>()?;
        }
        self.pending
            .pop_front()
            .context("missing authenticated end/file record (truncated archive)")
    }

    fn finish(mut self) -> Result<()> {
        ensure!(
            matches!(self.next()?, Record::End),
            "missing authenticated end marker"
        );
        ensure!(
            self.pending.is_empty() && self.reader.read(&mut [0u8; 1])? == 0,
            "trailing encrypted archive data"
        );
        Ok(())
    }
}

enum Staged {
    File(tempfile::NamedTempFile),
    Directory(tempfile::TempDir),
}

impl Staged {
    fn new(output: &Path, directory: bool) -> Result<Self> {
        fs::create_dir_all(parent(output))?;
        if directory {
            ensure!(!output.exists(), "output directory already exists");
            Ok(Self::Directory(
                tempfile::Builder::new()
                    .prefix(".fastener-decrypt-")
                    .tempdir_in(parent(output))?,
            ))
        } else {
            Ok(Self::File(
                tempfile::Builder::new()
                    .prefix(".fastener-decrypt-")
                    .tempfile_in(parent(output))?,
            ))
        }
    }

    fn writer(&self, entry: &Entry) -> Result<BufWriter<File>> {
        let file = match self {
            Self::File(temp) => temp.reopen()?,
            Self::Directory(temp) => {
                let destination = temp.path().join(&entry.path);
                fs::create_dir_all(parent(&destination))?;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(destination)?
            }
        };
        Ok(BufWriter::with_capacity(1024 * 1024, file))
    }

    fn publish(self, output: &Path) -> Result<()> {
        match self {
            Self::File(temp) => {
                temp.persist(output)
                    .context("could not publish decrypted file")?;
            }
            Self::Directory(temp) => {
                ensure!(!output.exists(), "output directory already exists");
                fs::rename(temp.path(), output).context("could not publish decrypted directory")?;
            }
        }
        Ok(())
    }
}

fn process_encrypted(
    input: &Path,
    output: Option<&Path>,
    password: &[u8],
    limits: EncryptedLimits,
    mut progress: impl FnMut(ProgressInfo),
) -> Result<EncryptedReport> {
    if let Some(output) = output {
        distinct(input, output)?;
    }
    let file = File::open(input)?;
    let archive_size = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let header = read_header(&mut reader)?;
    let directory = header[10] == 1;
    let crypto = Crypto::derive(header, password)?;
    let manifest = crypto.open(
        0,
        read_frame(&mut reader)?.context("missing encrypted manifest")?,
    )?;
    let (entries, total) = decode_manifest(&manifest, directory)?;
    ensure!(
        total <= limits.max_output_bytes,
        "encrypted archive exceeds the expanded-size limit"
    );
    let staged = output
        .map(|path| Staged::new(path, directory))
        .transpose()?;
    let mut records = Records {
        reader,
        crypto: &crypto,
        sequence: 1,
        pending: VecDeque::new(),
    };
    let phase = if output.is_some() {
        ProgressPhase::Decompressing
    } else {
        ProgressPhase::Verifying
    };
    let mut completed = 0u64;
    let mut chunks = 0usize;
    progress(ProgressInfo {
        phase,
        completed,
        total,
    });
    for entry in &entries {
        if entry.directory {
            if let Some(Staged::Directory(temp)) = &staged {
                fs::create_dir_all(temp.path().join(&entry.path))?;
            }
            continue;
        }
        let mut writer = staged.as_ref().map(|s| s.writer(entry)).transpose()?;
        let mut remaining = entry.size;
        let mut hasher = blake3::Hasher::new();
        while remaining > 0 {
            let Record::Data(data) = records.next()? else {
                bail!("unexpected file boundary in encrypted archive")
            };
            ensure!(
                data.len() as u64 <= remaining,
                "chunk exceeds manifest file size"
            );
            hasher.update_rayon(&data);
            if let Some(writer) = &mut writer {
                writer.write_all(&data)?;
            }
            remaining -= data.len() as u64;
            completed += data.len() as u64;
            chunks += 1;
            progress(ProgressInfo {
                phase,
                completed,
                total,
            });
        }
        let Record::Hash(hash) = records.next()? else {
            bail!("missing authenticated whole-file hash")
        };
        ensure!(
            hasher.finalize().as_bytes() == &hash,
            "whole-file checksum mismatch"
        );
        if let Some(writer) = &mut writer {
            writer.flush()?;
        }
    }
    records.finish()?;
    if let (Some(staged), Some(output)) = (staged, output) {
        staged.publish(output)?;
    }
    progress(ProgressInfo {
        phase,
        completed: total,
        total,
    });
    Ok(EncryptedReport {
        original_size: total,
        archive_size,
        files: entries.iter().filter(|e| !e.directory).count(),
        chunks,
        is_directory: directory,
    })
}

pub fn decompress_encrypted_with_progress(
    input: &Path,
    output: &Path,
    password: &[u8],
    progress: impl FnMut(ProgressInfo),
) -> Result<EncryptedReport> {
    decompress_encrypted_with_limits_and_progress(
        input,
        output,
        password,
        EncryptedLimits::default(),
        progress,
    )
}

pub fn decompress_encrypted_with_limits_and_progress(
    input: &Path,
    output: &Path,
    password: &[u8],
    limits: EncryptedLimits,
    progress: impl FnMut(ProgressInfo),
) -> Result<EncryptedReport> {
    process_encrypted(input, Some(output), password, limits, progress)
}

/// Verification never creates plaintext files on disk.
pub fn verify_encrypted_with_progress(
    input: &Path,
    password: &[u8],
    progress: impl FnMut(ProgressInfo),
) -> Result<EncryptedReport> {
    verify_encrypted_with_limits_and_progress(input, password, EncryptedLimits::default(), progress)
}

pub fn verify_encrypted_with_limits_and_progress(
    input: &Path,
    password: &[u8],
    limits: EncryptedLimits,
    progress: impl FnMut(ProgressInfo),
) -> Result<EncryptedReport> {
    process_encrypted(input, None, password, limits, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> Crypto {
        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(ENCRYPTED_MAGIC);
        header[8] = 1;
        header[11] = 1;
        header[12..16].copy_from_slice(&8192u32.to_le_bytes());
        header[32..48].copy_from_slice(&[7; 16]);
        Crypto {
            header,
            cipher: XChaCha20Poly1305::new_from_slice(&[19; 32]).unwrap(),
        }
    }

    #[test]
    fn record_authentication_binds_header_sequence_and_length() {
        let mut crypto = crypto();
        let ciphertext = crypto.seal(4, Zeroizing::new(b"payload".to_vec())).unwrap();
        assert_eq!(&*crypto.open(4, ciphertext.clone()).unwrap(), b"payload");
        assert!(crypto.open(5, ciphertext.clone()).is_err());
        crypto.header[10] ^= 1;
        assert!(crypto.open(4, ciphertext.clone()).is_err());
        crypto.header[10] ^= 1;
        let mut changed_length = ciphertext;
        changed_length.push(0);
        assert!(crypto.open(4, changed_length).is_err());
        assert_ne!(crypto.nonce(0), crypto.nonce(u64::MAX));
        assert_eq!(&crypto.nonce(0x0102)[16..], &0x0102u64.to_le_bytes());
        let mut last = u64::MAX;
        assert!(next_sequence(&mut last).is_err());
    }

    #[test]
    fn manifest_rejects_traversal_devices_duplicates_and_file_parent() {
        for path in [
            "../outside",
            "/absolute",
            "C:/escape",
            "a\\b",
            "a//b",
            "a/./b",
            "a:stream",
            "CON",
            "nul.txt",
            "COM1.txt",
            "COM¹",
            "x.",
            "x ",
            "x\0y",
        ] {
            assert!(safe_path(path).is_err(), "accepted {path:?}");
        }
        assert!(safe_path("資料/原稿.txt").is_ok());
        let entries = vec![
            Entry {
                path: "a".into(),
                directory: false,
                size: 1,
            },
            Entry {
                path: "a/b".into(),
                directory: false,
                size: 1,
            },
        ];
        assert!(validate_entries(&entries, true).is_err());
        let entries = vec![
            Entry {
                path: "File".into(),
                directory: false,
                size: 0,
            },
            Entry {
                path: "file".into(),
                directory: false,
                size: 0,
            },
        ];
        assert!(validate_entries(&entries, true).is_err());
        assert!(decode_manifest(&[b'M', 255, 255, 255, 255], true).is_err());
        assert!(decode_manifest(&[b'M', 0, 0, 0, 0, 0], true).is_err());
    }

    #[test]
    fn decoded_and_stored_record_sizes_are_bounded() {
        let mut bytes = vec![b'D'];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.push(0);
        assert!(decode_record(Zeroizing::new(bytes)).is_err());
        assert!(decode_record(Zeroizing::new(vec![b'D', 1, 0, 0, 0, 0])).is_err());
        assert!(read_frame(&mut Cursor::new(u32::MAX.to_le_bytes())).is_err());
        assert!(read_frame(&mut Cursor::new([0u8])).is_err());
        let mut header = crypto().header;
        header[11] = 255;
        assert!(validate_header(&header).is_err());
    }
}
