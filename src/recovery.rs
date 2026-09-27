//! FSTPAR01 sidecar recovery. Fixed-offset erasures, not byte insertion/deletion repair.
use crate::file_ops::update_whole_hash;
use crate::{ProgressInfo, ProgressPhase};
use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;
use reed_solomon_erasure::galois_8::ReedSolomon;
use std::{
    fs::File,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"FSTPAR01";
const HEADER: usize = 128;
const DATA: usize = 20;
const PARITY: usize = 2;
const TOTAL: usize = DATA + PARITY;
const INDEX: usize = 8 + TOTAL * 32 + 32;
const MAX_GROUPS: u64 = 1_000_000;

// Reed-Solomon operates independently at each byte offset. Split each shard
// into matching stripes, borrowing disjoint slices rather than copying data.
fn code_shards(rs: &ReedSolomon, shards: &mut [Vec<u8>], present: Option<&[bool]>) -> Result<()> {
    let len = shards[0].len();
    let stripe = if len >= 256 * 1024 { 64 * 1024 } else { len };
    let mut lanes: Vec<Vec<(&mut [u8], bool)>> = (0..len.div_ceil(stripe))
        .map(|_| Vec::with_capacity(TOTAL))
        .collect();
    for (i, shard) in shards.iter_mut().enumerate() {
        for (lane, part) in lanes.iter_mut().zip(shard.chunks_mut(stripe)) {
            lane.push((part, present.is_none_or(|flags| flags[i])));
        }
    }
    let process = |mut lane: Vec<(&mut [u8], bool)>| -> Result<()> {
        if present.is_some() {
            rs.reconstruct_data(&mut lane)?;
        } else {
            let mut slices: Vec<_> = lane.into_iter().map(|(part, _)| part).collect();
            rs.encode(&mut slices)?;
        }
        Ok(())
    };
    if lanes.len() == 1 {
        process(lanes.pop().unwrap())
    } else {
        lanes.into_par_iter().try_for_each(process)
    }
}

fn shard_hashes(shards: &[Vec<u8>]) -> Vec<blake3::Hash> {
    if shards[0].len() >= 256 * 1024 {
        shards.par_iter().map(|s| blake3::hash(s)).collect()
    } else {
        shards.iter().map(|s| blake3::hash(s)).collect()
    }
}

#[derive(Debug, Clone)]
pub struct RecoveryInfo {
    pub original_bytes: u64,
    pub recovery_bytes: u64,
    pub shard_bytes: usize,
    pub groups: u64,
    pub encrypted: bool,
}

#[derive(Debug)]
pub struct RecoveryReport {
    pub info: RecoveryInfo,
    pub repaired_shards: u64,
    pub damaged_parity_shards: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    len: u64,
    shard: usize,
    groups: u64,
    hash: [u8; 32],
    kind: u8,
}

impl Header {
    fn stride(&self) -> u64 {
        (2 * INDEX + PARITY * self.shard) as u64
    }
    fn size(&self) -> u64 {
        2 * HEADER as u64 + self.groups * self.stride()
    }
    fn info(&self) -> RecoveryInfo {
        RecoveryInfo {
            original_bytes: self.len,
            recovery_bytes: self.size(),
            shard_bytes: self.shard,
            groups: self.groups,
            encrypted: self.kind == 3,
        }
    }
    fn bytes(&self) -> [u8; HEADER] {
        let mut out = [0u8; HEADER];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&1u32.to_le_bytes());
        out[12..16].copy_from_slice(&(self.shard as u32).to_le_bytes());
        out[16..24].copy_from_slice(&self.len.to_le_bytes());
        out[24..32].copy_from_slice(&self.groups.to_le_bytes());
        out[32..64].copy_from_slice(&self.hash);
        out[64] = self.kind;
        let hash = blake3::hash(&out[..96]);
        out[96..].copy_from_slice(hash.as_bytes());
        out
    }
    fn parse(bytes: &[u8; HEADER]) -> Result<Self> {
        ensure!(
            &bytes[..8] == MAGIC && bytes[8..12] == 1u32.to_le_bytes(),
            "invalid recovery header"
        );
        ensure!(
            blake3::hash(&bytes[..96]).as_bytes() == &bytes[96..],
            "recovery header checksum mismatch"
        );
        let shard = u32::from_le_bytes(bytes[12..16].try_into()?) as usize;
        ensure!(
            (4096..=1024 * 1024).contains(&shard) && shard.is_power_of_two(),
            "invalid recovery shard size"
        );
        let len = u64::from_le_bytes(bytes[16..24].try_into()?);
        let groups = u64::from_le_bytes(bytes[24..32].try_into()?);
        ensure!(
            groups == len.div_ceil((DATA * shard) as u64) && groups <= MAX_GROUPS,
            "invalid recovery group count"
        );
        ensure!(
            bytes[64] <= 3 && bytes[65..96].iter().all(|&b| b == 0),
            "unsupported recovery metadata"
        );
        Ok(Self {
            len,
            shard,
            groups,
            hash: bytes[32..64].try_into()?,
            kind: bytes[64],
        })
    }
}

/// Exact sidecar size before creation; shard size is adaptive between 4 KiB and 1 MiB.
pub fn recovery_plan(original_bytes: u64) -> Result<RecoveryInfo> {
    let desired = original_bytes
        .div_ceil(DATA as u64)
        .clamp(4096, 1024 * 1024);
    let shard = (desired as usize).next_power_of_two();
    let groups = original_bytes.div_ceil((DATA * shard) as u64);
    ensure!(
        groups <= MAX_GROUPS,
        "archive exceeds recovery format limit"
    );
    Ok(Header {
        len: original_bytes,
        shard,
        groups,
        hash: [0; 32],
        kind: 0,
    }
    .info())
}

pub fn recovery_path(input: &Path) -> PathBuf {
    let mut name = input.as_os_str().to_owned();
    name.push(".par");
    PathBuf::from(name)
}

pub fn repaired_path(input: &Path) -> PathBuf {
    let mut name = input.file_stem().unwrap_or(input.as_os_str()).to_owned();
    name.push(".repaired");
    if let Some(ext) = input.extension() {
        name.push(".");
        name.push(ext);
    }
    input.with_file_name(name)
}

fn distinct(source: &Path, output: &Path) -> Result<()> {
    if output.exists() {
        ensure!(
            source.canonicalize()? != output.canonicalize()?,
            "recovery output must differ from its inputs"
        );
    }
    Ok(())
}

fn temporary(output: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(tempfile::Builder::new()
        .prefix(".fastener-recovery-")
        .tempfile_in(parent)?)
}

fn kind(file: &mut File) -> Result<u8> {
    let mut magic = [0; 8];
    let len = file.read(&mut magic)?;
    file.rewind()?;
    if len == 8 {
        if &magic == b"FASTENR1" {
            return Ok(0);
        }
        if &magic == crate::DIRECTORY_MAGIC {
            return Ok(1);
        }
        if &magic == crate::ENCRYPTED_MAGIC {
            return Ok(3);
        }
    }
    if len >= 4 && matches!(&magic[..4], b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08") {
        return Ok(2);
    }
    bail!("recovery supports Fastener and ZIP archives only")
}

fn progress(
    callback: &mut impl FnMut(ProgressInfo),
    phase: ProgressPhase,
    completed: u64,
    total: u64,
) {
    callback(ProgressInfo {
        phase,
        completed,
        total,
    });
}

pub fn create_recovery_with_progress(
    input: &Path,
    output: &Path,
    mut callback: impl FnMut(ProgressInfo),
) -> Result<RecoveryInfo> {
    distinct(input, output)?;
    let mut source = File::open(input)?;
    ensure!(
        source.metadata()?.is_file(),
        "input must be a regular archive file"
    );
    let plan = recovery_plan(source.metadata()?.len())?;
    let mut header = Header {
        len: plan.original_bytes,
        shard: plan.shard_bytes,
        groups: plan.groups,
        hash: [0; 32],
        kind: kind(&mut source)?,
    };
    let mut temporary = temporary(output)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, temporary.as_file_mut());
    writer.write_all(&[0; HEADER])?;
    let rs = ReedSolomon::new(DATA, PARITY)?;
    let mut hasher = blake3::Hasher::new();
    let mut completed = 0;
    progress(
        &mut callback,
        ProgressPhase::RecoveryCreating,
        0,
        header.len,
    );
    let mut shards = vec![vec![0u8; header.shard]; TOTAL];
    for group in 0..header.groups {
        for shard in shards.iter_mut().take(DATA) {
            let amount = (header.len - completed).min(header.shard as u64) as usize;
            source.read_exact(&mut shard[..amount])?;
            shard[amount..].fill(0);
            update_whole_hash(&mut hasher, &shard[..amount]);
            completed += amount as u64;
        }
        code_shards(&rs, &mut shards, None)?;
        let mut index = [0u8; INDEX];
        index[..8].copy_from_slice(&group.to_le_bytes());
        for (i, hash) in shard_hashes(&shards).iter().enumerate() {
            index[8 + i * 32..8 + (i + 1) * 32].copy_from_slice(hash.as_bytes());
        }
        let checksum = blake3::hash(&index[..INDEX - 32]);
        index[INDEX - 32..].copy_from_slice(checksum.as_bytes());
        writer.write_all(&index)?;
        for shard in &shards[DATA..] {
            writer.write_all(shard)?;
        }
        writer.write_all(&index)?;
        progress(
            &mut callback,
            ProgressPhase::RecoveryCreating,
            completed,
            header.len,
        );
    }
    ensure!(
        source.read(&mut [0u8; 1])? == 0,
        "input grew while creating recovery data"
    );
    header.hash = *hasher.finalize().as_bytes();
    writer.write_all(&header.bytes())?;
    writer.seek(SeekFrom::Start(0))?;
    writer.write_all(&header.bytes())?;
    writer.flush()?;
    drop(writer);
    temporary
        .persist(output)
        .context("could not publish recovery file")?;
    Ok(header.info())
}

fn read_at(file: &mut File, offset: u64, bytes: &mut [u8]) -> Result<bool> {
    file.seek(SeekFrom::Start(offset))?;
    match file.read_exact(bytes) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn read_header(file: &mut File) -> Result<Header> {
    let len = file.metadata()?.len();
    ensure!(len >= HEADER as u64, "truncated recovery header");
    let mut bytes = [0; HEADER];
    let first = if read_at(file, 0, &mut bytes)? {
        Header::parse(&bytes).ok()
    } else {
        None
    };
    let last_offset = first
        .as_ref()
        .map(|h| h.size() - HEADER as u64)
        .unwrap_or(len - HEADER as u64);
    let last = if read_at(file, last_offset, &mut bytes)? {
        Header::parse(&bytes).ok()
    } else {
        None
    };
    match (first, last) {
        (Some(a), Some(b)) => {
            ensure!(a == b, "conflicting recovery headers");
            Ok(a)
        }
        (Some(a), None) => Ok(a),
        (None, Some(b)) => {
            ensure!(b.size() == len, "misplaced recovery footer");
            Ok(b)
        }
        _ => bail!("both recovery headers are damaged"),
    }
}

pub fn recovery_info(path: &Path) -> Result<RecoveryInfo> {
    Ok(read_header(&mut File::open(path)?)?.info())
}

fn read_index(file: &mut File, header: &Header, group: u64) -> Result<[u8; INDEX]> {
    let start = HEADER as u64 + group * header.stride();
    let mut copies = Vec::with_capacity(2);
    for offset in [start, start + INDEX as u64 + (PARITY * header.shard) as u64] {
        let mut index = [0; INDEX];
        if read_at(file, offset, &mut index)?
            && index[..8] == group.to_le_bytes()
            && blake3::hash(&index[..INDEX - 32]).as_bytes() == &index[INDEX - 32..]
        {
            copies.push(index);
        }
    }
    ensure!(
        !copies.is_empty(),
        "both recovery indexes are damaged in group {group}"
    );
    ensure!(
        copies.len() != 2 || copies[0] == copies[1],
        "conflicting recovery indexes in group {group}"
    );
    Ok(copies[0])
}

/// Repair to a sibling temporary file; authenticate/verify the entire archive before publication.
/// The damaged input is never overwritten, even when the output path is explicitly provided.
pub fn repair_with_progress(
    input: &Path,
    recovery: &Path,
    output: &Path,
    password: Option<&[u8]>,
    callback: impl FnMut(ProgressInfo),
) -> Result<RecoveryReport> {
    repair_with_zip_limits_and_progress(
        input,
        recovery,
        output,
        password,
        crate::ZipLimits::default(),
        callback,
    )
}

pub fn repair_with_zip_limits_and_progress(
    input: &Path,
    recovery: &Path,
    output: &Path,
    password: Option<&[u8]>,
    zip_limits: crate::ZipLimits,
    callback: impl FnMut(ProgressInfo),
) -> Result<RecoveryReport> {
    repair_with_archive_limits_and_progress(
        input,
        recovery,
        output,
        password,
        zip_limits,
        crate::EncryptedLimits::default(),
        callback,
    )
}

pub fn repair_with_archive_limits_and_progress(
    input: &Path,
    recovery: &Path,
    output: &Path,
    password: Option<&[u8]>,
    zip_limits: crate::ZipLimits,
    encrypted_limits: crate::EncryptedLimits,
    mut callback: impl FnMut(ProgressInfo),
) -> Result<RecoveryReport> {
    distinct(input, output)?;
    distinct(recovery, output)?;
    let mut parity_file = File::open(recovery)?;
    let header = read_header(&mut parity_file)?;
    ensure!(
        header.kind != 3 || password.is_some_and(|p| !p.is_empty()),
        "encrypted recovery requires a password for final authentication"
    );
    ensure!(
        header.kind == 3 || password.is_none(),
        "password is only supported for encrypted archives"
    );
    let mut source = File::open(input)?;
    ensure!(source.metadata()?.is_file(), "input must be a regular file");
    let mut temporary = temporary(output)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, temporary.as_file_mut());
    let rs = ReedSolomon::new(DATA, PARITY)?;
    let mut hasher = blake3::Hasher::new();
    let mut completed = 0u64;
    let mut repaired = 0u64;
    let mut damaged_parity = 0u64;
    progress(
        &mut callback,
        ProgressPhase::RecoveryRepairing,
        0,
        header.len,
    );
    let mut shards = vec![vec![0u8; header.shard]; TOTAL];
    for group in 0..header.groups {
        let index = read_index(&mut parity_file, &header, group)?;
        let mut complete_shards = [false; TOTAL];
        for (i, shard) in shards.iter_mut().enumerate() {
            let complete = if i < DATA {
                let offset = (group * DATA as u64 + i as u64) * header.shard as u64;
                let amount = header.len.saturating_sub(offset).min(header.shard as u64) as usize;
                shard[amount..].fill(0);
                // Padding is known zero even when the damaged input has been truncated.
                amount == 0 || read_at(&mut source, offset, &mut shard[..amount])?
            } else {
                let offset = HEADER as u64
                    + group * header.stride()
                    + INDEX as u64
                    + ((i - DATA) * header.shard) as u64;
                read_at(&mut parity_file, offset, shard)?
            };
            complete_shards[i] = complete;
        }
        let mut present = [false; TOTAL];
        for (i, hash) in shard_hashes(&shards).iter().enumerate() {
            let valid =
                complete_shards[i] && hash.as_bytes() == &index[8 + i * 32..8 + (i + 1) * 32];
            if !valid {
                if i < DATA {
                    repaired += 1;
                } else {
                    damaged_parity += 1;
                }
            }
            present[i] = valid;
        }
        // Data may be intact even if all parity is lost. Otherwise total erasures must fit.
        if present[..DATA].iter().any(|&valid| !valid) {
            ensure!(
                present.iter().filter(|&&valid| !valid).count() <= PARITY,
                "recovery capacity exceeded in group {group} (maximum 2 missing data/parity shards combined)"
            );
            code_shards(&rs, &mut shards, Some(&present))
                .context("Reed-Solomon reconstruction failed")?;
        }
        for (i, hash) in shard_hashes(&shards[..DATA]).iter().enumerate() {
            ensure!(
                hash.as_bytes() == &index[8 + i * 32..8 + (i + 1) * 32],
                "reconstructed shard checksum mismatch"
            );
        }
        for shard in shards.iter().take(DATA) {
            let amount = (header.len - completed).min(header.shard as u64) as usize;
            writer.write_all(&shard[..amount])?;
            update_whole_hash(&mut hasher, &shard[..amount]);
            completed += amount as u64;
        }
        progress(
            &mut callback,
            ProgressPhase::RecoveryRepairing,
            completed,
            header.len,
        );
    }
    ensure!(
        completed == header.len && hasher.finalize().as_bytes() == &header.hash,
        "repaired archive whole-file checksum mismatch"
    );
    // Reused buffers are no longer needed; release them before the independent
    // archive verifier maps/decodes the repaired file or derives an encryption key.
    drop(shards);
    writer.flush()?;
    drop(writer);
    ensure!(
        kind(&mut File::open(temporary.path())?)? == header.kind,
        "reconstructed archive kind does not match recovery metadata"
    );
    match header.kind {
        0 => {
            crate::verify_file_with_progress(temporary.path(), &mut callback)?;
        }
        1 => {
            crate::verify_directory_bundle_with_progress(temporary.path(), &mut callback)?;
        }
        2 => {
            crate::verify_zip_file_with_limits_and_progress(
                temporary.path(),
                zip_limits,
                &mut callback,
            )?;
        }
        3 => {
            crate::verify_encrypted_with_limits_and_progress(
                temporary.path(),
                password.unwrap(),
                encrypted_limits,
                &mut callback,
            )?;
        }
        _ => unreachable!(),
    }
    temporary
        .persist(output)
        .context("could not publish repaired archive")?;
    Ok(RecoveryReport {
        info: header.info(),
        repaired_shards: repaired,
        damaged_parity_shards: damaged_parity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn striped_encoding_and_reconstruction_match_serial_reference() {
        let rs = ReedSolomon::new(DATA, PARITY).unwrap();
        for len in [4096, 256 * 1024 + 17, 1024 * 1024] {
            let mut reference: Vec<Vec<u8>> = (0..TOTAL)
                .map(|i| {
                    (0..len)
                        .map(|j| ((i * 17 + j * 31 + j / 113) % 251) as u8)
                        .collect()
                })
                .collect();
            let mut striped = reference.clone();
            rs.encode(&mut reference).unwrap();
            code_shards(&rs, &mut striped, None).unwrap();
            assert_eq!(striped, reference);
            for missing in [[0, 1], [19, 20]] {
                let mut damaged = reference.clone();
                let mut present = [true; TOTAL];
                for i in missing {
                    damaged[i].fill(0xAB);
                    present[i] = false;
                }
                code_shards(&rs, &mut damaged, Some(&present)).unwrap();
                assert_eq!(&damaged[..DATA], &reference[..DATA]);
            }
        }
        let mut shards = vec![vec![71u8; 256 * 1024]; TOTAL];
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| code_shards(&rs, &mut shards, None))
            .unwrap();
        assert!(rs.verify(&shards).unwrap());
    }

    #[test]
    fn forged_header_limits_are_checked_even_with_valid_hashes() {
        let base = Header {
            len: 100_000,
            shard: 8192,
            groups: 1,
            hash: [7; 32],
            kind: 0,
        };
        assert!(Header::parse(&base.bytes()).is_ok());
        for bad in [
            Header {
                shard: 0,
                ..base.clone()
            },
            Header {
                shard: 3,
                ..base.clone()
            },
            Header {
                shard: 2 * 1024 * 1024,
                ..base.clone()
            },
            Header {
                groups: u64::MAX,
                ..base.clone()
            },
            Header {
                len: u64::MAX,
                ..base.clone()
            },
            Header {
                kind: 4,
                ..base.clone()
            },
        ] {
            assert!(Header::parse(&bad.bytes()).is_err());
        }
        assert!(recovery_plan(u64::MAX).is_err());
    }
}
