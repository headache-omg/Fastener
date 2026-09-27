use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use fastener::{
    CompressOptions, DIRECTORY_MAGIC, ZipLimits, compress_directory_bundle_with_progress,
    compress_file, compress_zip_file, decompress_directory_bundle_with_progress, decompress_file,
    extract_zip_file_with_limits, verify_directory_bundle_with_progress, verify_file,
    verify_zip_file_with_limits,
};
use fastener::{
    ENCRYPTED_MAGIC, compress_encrypted_with_progress, decompress_encrypted_with_progress,
    encrypted_is_directory, repair_with_zip_limits_and_progress, verify_encrypted_with_progress,
};
use rayon::ThreadPoolBuilder;
use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Parser, Debug)]
#[command(name = "fastener", version, about = "Parallel Fastener archiver")]
struct Cli {
    /// Number of CPU worker threads (defaults to Rayon auto-detection).
    #[arg(long, global = true, value_name = "N")]
    threads: Option<usize>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create an optional .par recovery file for a finished FST or ZIP archive.
    RecoveryCreate {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
        /// Show exact additional storage without writing anything.
        #[arg(long)]
        estimate: bool,
    },
    /// Repair an archive using its .par file, then verify/authenticate it.
    Repair {
        input: PathBuf,
        #[arg(long)]
        recovery: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
        #[arg(long)]
        password_file: Option<PathBuf>,
        #[arg(long, value_name = "N")]
        zip_max_entries: Option<usize>,
        #[arg(long, value_name = "BYTES")]
        zip_max_output_bytes: Option<u64>,
    },
    /// Compress one file into a .fst archive.
    Compress {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 8 * 1024 * 1024, value_name = "BYTES")]
        chunk_size: usize,
        /// 0 = fastest LZ4, 1 = balanced Zstd, 12 = dense Zstd.
        #[arg(short, long, default_value_t = 1, value_parser = clap::value_parser!(i32).range(0..=22))]
        level: i32,
        #[arg(short, long)]
        force: bool,
        /// Encrypt the file/folder; prompts for a hidden password twice.
        #[arg(long)]
        encrypt: bool,
        /// Read a UTF-8 password from a protected file instead of prompting.
        #[arg(long, requires = "encrypt")]
        password_file: Option<PathBuf>,
    },
    /// Compress one file into a conventional Deflate/Zip64 .zip archive.
    ZipCompress {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Deflate compression level (0 = no compression work, 9 = densest).
        #[arg(short, long, default_value_t = 1, value_parser = clap::value_parser!(i32).range(0..=9))]
        level: i32,
        #[arg(short, long)]
        force: bool,
    },
    /// Decompress a .fst archive.
    Decompress {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// Maximum ZIP entries to accept (default: 100000).
        #[arg(long, value_name = "N")]
        zip_max_entries: Option<usize>,
        /// Maximum ZIP expanded bytes (default: 64 GiB).
        #[arg(long, value_name = "BYTES")]
        zip_max_output_bytes: Option<u64>,
    },
    /// Fully decode an archive in memory and verify every checksum.
    Verify {
        input: PathBuf,
        #[arg(long)]
        password_file: Option<PathBuf>,
        #[arg(long, value_name = "N")]
        zip_max_entries: Option<usize>,
        #[arg(long, value_name = "BYTES")]
        zip_max_output_bytes: Option<u64>,
    },
    /// Measure compression and decompression on one input file.
    Benchmark {
        input: PathBuf,
        #[arg(short, long, default_value_t = 3)]
        iterations: usize,
        #[arg(long, default_value_t = 8 * 1024 * 1024, value_name = "BYTES")]
        chunk_size: usize,
        /// 0 = fastest LZ4, 1 = balanced Zstd, 12 = dense Zstd.
        #[arg(short, long, default_value_t = 1, value_parser = clap::value_parser!(i32).range(0..=22))]
        level: i32,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Some(threads) = cli.threads {
        ensure!(threads > 0, "--threads must be greater than zero");
        ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .context("could not configure worker threads")?;
    }

    match cli.command {
        Command::RecoveryCreate {
            input,
            output,
            force,
            estimate,
        } => {
            let plan = fastener::recovery_plan(fs::metadata(&input)?.len())?;
            println!(
                "additional  : {} bytes ({:.2}%)",
                plan.recovery_bytes,
                100.0 * plan.recovery_bytes as f64 / plan.original_bytes.max(1) as f64
            );
            println!(
                "recovery    : 20 data + 2 parity, {} bytes/shard",
                plan.shard_bytes
            );
            if !estimate {
                let output = output.unwrap_or_else(|| fastener::recovery_path(&input));
                prepare_output(&input, &output, force)?;
                fastener::create_recovery_with_progress(&input, &output, |_| {})?;
                println!("output      : {}", output.display());
            }
        }
        Command::Repair {
            input,
            recovery,
            output,
            force,
            password_file,
            zip_max_entries,
            zip_max_output_bytes,
        } => {
            let recovery = recovery.unwrap_or_else(|| fastener::recovery_path(&input));
            let info = fastener::recovery_info(&recovery)?;
            ensure!(
                info.encrypted || password_file.is_none(),
                "--password-file requires an encrypted archive"
            );
            let password = if info.encrypted {
                Some(read_password(password_file.as_deref(), false)?)
            } else {
                None
            };
            let output = output.unwrap_or_else(|| fastener::repaired_path(&input));
            prepare_output(&input, &output, force)?;
            let report = repair_with_zip_limits_and_progress(
                &input,
                &recovery,
                &output,
                password.as_ref().map(|p| p.as_slice()),
                zip_limits(zip_max_entries, zip_max_output_bytes),
                |_| {},
            )?;
            println!(
                "repaired    : {} data shards; {} damaged parity shards",
                report.repaired_shards, report.damaged_parity_shards
            );
            println!("validation  : whole-file hash and archive verification/authentication OK");
            println!("output      : {}", output.display());
        }
        Command::Compress {
            input,
            output,
            chunk_size,
            level,
            force,
            encrypt,
            password_file,
        } => {
            let output = output.unwrap_or_else(|| default_compressed_path(&input));
            prepare_output(&input, &output, force)?;
            let started = Instant::now();
            let options = CompressOptions {
                target_chunk_size: chunk_size,
                compression_level: level,
            };
            if encrypt {
                let password = read_password(password_file.as_deref(), true)?;
                let report =
                    compress_encrypted_with_progress(&input, &output, &options, &password, |_| {})?;
                println!(
                    "encrypted   : {} files, {} chunks",
                    report.files, report.chunks
                );
                println!(
                    "size        : {} -> {}",
                    human_bytes(report.original_size),
                    human_bytes(report.archive_size)
                );
                println!("output      : {}", output.display());
                return Ok(());
            }
            if input.is_dir() {
                let report =
                    compress_directory_bundle_with_progress(&input, &output, &options, |_| {})?;
                println!("compressed  : {} files", report.files);
                println!(
                    "size        : {} -> {} ({:.1}%)",
                    human_bytes(report.original_size),
                    human_bytes(report.archive_size),
                    report.archive_size as f64 / report.original_size.max(1) as f64 * 100.0
                );
                println!(
                    "throughput  : {}",
                    human_rate(report.original_size, started.elapsed())
                );
                println!("output      : {}", output.display());
                return Ok(());
            }
            let stats = compress_file(&input, &output, &options)?;
            let elapsed = started.elapsed();
            println!(
                "compressed  : {} -> {} ({:.1}%)",
                human_bytes(stats.original_size),
                human_bytes(stats.archive_size),
                stats.ratio() * 100.0
            );
            println!(
                "chunks      : {} ({} stored raw)",
                stats.chunk_count, stats.raw_chunks
            );
            println!("throughput  : {}", human_rate(stats.original_size, elapsed));
            println!("output      : {}", output.display());
        }
        Command::ZipCompress {
            input,
            output,
            level,
            force,
        } => {
            let output = output.unwrap_or_else(|| input.with_extension("zip"));
            prepare_output(&input, &output, force)?;
            let started = Instant::now();
            let report = compress_zip_file(&input, &output, level)?;
            let elapsed = started.elapsed();
            println!("compressed  : {} entry", report.entries);
            println!(
                "size        : {} -> {} ({:.1}%)",
                human_bytes(report.original_size),
                human_bytes(report.archive_size),
                report.archive_size as f64 / report.original_size.max(1) as f64 * 100.0
            );
            println!("method      : Deflate level {level}, Zip64 when required");
            println!(
                "throughput  : {}",
                human_rate(report.original_size, elapsed)
            );
            println!("output      : {}", output.display());
        }
        Command::Decompress {
            input,
            output,
            force,
            password_file,
            zip_max_entries,
            zip_max_output_bytes,
        } => {
            let started = Instant::now();
            let kind = archive_kind(&input)?;
            ensure!(
                password_file.is_none() || kind == CliArchiveKind::Encrypted,
                "--password-file is only supported for encrypted Fastener archives"
            );
            ensure!(
                (zip_max_entries.is_none() && zip_max_output_bytes.is_none())
                    || kind == CliArchiveKind::Zip,
                "ZIP limits are only supported for ZIP archives"
            );
            if kind == CliArchiveKind::Encrypted {
                let directory = encrypted_is_directory(&input)?;
                let output = output.unwrap_or_else(|| {
                    if directory {
                        default_directory_output(&input)
                    } else {
                        default_decompressed_path(&input)
                    }
                });
                prepare_output(&input, &output, force)?;
                let password = read_password(password_file.as_deref(), false)?;
                let report =
                    decompress_encrypted_with_progress(&input, &output, &password, |_| {})?;
                println!(
                    "decrypted   : {} files, {}",
                    report.files,
                    human_bytes(report.original_size)
                );
                println!("authentication: OK");
                println!("output      : {}", output.display());
                return Ok(());
            }
            if archive_kind(&input)? == CliArchiveKind::Zip {
                let output = output.unwrap_or_else(|| default_zip_directory(&input));
                ensure!(!output.exists(), "{} already exists", output.display());
                let report = extract_zip_file_with_limits(
                    &input,
                    &output,
                    zip_limits(zip_max_entries, zip_max_output_bytes),
                )?;
                println!("extracted   : {} entries", report.entries);
                println!("size        : {}", human_bytes(report.uncompressed_size));
                println!(
                    "workers     : {} used / {} available (per entry)",
                    report.worker_threads,
                    rayon::current_num_threads()
                );
                println!(
                    "throughput  : {}",
                    human_rate(report.uncompressed_size, started.elapsed())
                );
                println!("output      : {}", output.display());
            } else if archive_kind(&input)? == CliArchiveKind::Directory {
                let output = output.unwrap_or_else(|| default_directory_output(&input));
                ensure!(!output.exists(), "{} already exists", output.display());
                let report = decompress_directory_bundle_with_progress(&input, &output, |_| {})?;
                println!("extracted   : {} files", report.files);
                println!("size        : {}", human_bytes(report.original_size));
                println!(
                    "workers     : {} used / {} available",
                    report.worker_threads,
                    rayon::current_num_threads()
                );
                println!(
                    "throughput  : {}",
                    human_rate(report.original_size, started.elapsed())
                );
                println!("checksum    : OK");
                println!("output      : {}", output.display());
            } else {
                let output = output.unwrap_or_else(|| default_decompressed_path(&input));
                prepare_output(&input, &output, force)?;
                let report = decompress_file(&input, &output)?;
                println!(
                    "decompressed: {} -> {}",
                    human_bytes(report.archive_size),
                    human_bytes(report.original_size)
                );
                println!(
                    "workers     : {} used / {} available",
                    report.worker_threads,
                    rayon::current_num_threads()
                );
                println!(
                    "throughput  : {}",
                    human_rate(report.original_size, started.elapsed())
                );
                println!("checksum    : OK");
                println!("output      : {}", output.display());
            }
        }
        Command::Verify {
            input,
            password_file,
            zip_max_entries,
            zip_max_output_bytes,
        } => {
            let started = Instant::now();
            let kind = archive_kind(&input)?;
            ensure!(
                password_file.is_none() || kind == CliArchiveKind::Encrypted,
                "--password-file is only supported for encrypted Fastener archives"
            );
            ensure!(
                (zip_max_entries.is_none() && zip_max_output_bytes.is_none())
                    || kind == CliArchiveKind::Zip,
                "ZIP limits are only supported for ZIP archives"
            );
            if kind == CliArchiveKind::Encrypted {
                let password = read_password(password_file.as_deref(), false)?;
                let report = verify_encrypted_with_progress(&input, &password, |_| {})?;
                println!(
                    "verified    : {} files, {}",
                    report.files,
                    human_bytes(report.original_size)
                );
                println!("authentication and checksum: OK");
                return Ok(());
            }
            if archive_kind(&input)? == CliArchiveKind::Zip {
                let report = verify_zip_file_with_limits(
                    &input,
                    zip_limits(zip_max_entries, zip_max_output_bytes),
                )?;
                println!("archive     : {}", input.display());
                println!("entries     : {}", report.entries);
                println!("expanded    : {}", human_bytes(report.uncompressed_size));
                println!("ZIP read    : OK ({:.3}s)", started.elapsed().as_secs_f64());
                println!(
                    "throughput  : {}",
                    human_rate(report.uncompressed_size, started.elapsed())
                );
            } else if archive_kind(&input)? == CliArchiveKind::Directory {
                let report = verify_directory_bundle_with_progress(&input, |_| {})?;
                println!("archive     : {}", input.display());
                println!("files       : {}", report.files);
                println!("original    : {}", human_bytes(report.original_size));
                println!("archive size: {}", human_bytes(report.archive_size));
                println!("checksum    : OK ({:.3}s)", started.elapsed().as_secs_f64());
            } else {
                let report = verify_file(&input)?;
                println!("archive     : {}", input.display());
                println!("chunks      : {}", report.chunk_count);
                println!("original    : {}", human_bytes(report.original_size));
                println!("archive size: {}", human_bytes(report.archive_size));
                println!("checksum    : OK ({:.3}s)", started.elapsed().as_secs_f64());
            }
        }
        Command::Benchmark {
            input,
            iterations,
            chunk_size,
            level,
        } => {
            ensure!(iterations > 0, "--iterations must be greater than zero");
            let source_size = fs::metadata(&input)?.len();
            println!(
                "input       : {} ({})",
                input.display(),
                human_bytes(source_size)
            );
            println!("iterations  : {iterations}");
            let options = CompressOptions {
                target_chunk_size: chunk_size,
                compression_level: level,
            };
            let mut compress_time = Duration::ZERO;
            let mut decompress_time = Duration::ZERO;
            let mut final_stats = None;
            let temporary = tempfile::Builder::new()
                .prefix("fastener-benchmark-")
                .tempdir()?;
            let archive_path = temporary.path().join("archive.fst");
            let restored_path = temporary.path().join("restored.bin");
            let source_hash = hash_file(&input)?;
            for _ in 0..iterations {
                let started = Instant::now();
                let stats = compress_file(&input, &archive_path, &options)?;
                compress_time += started.elapsed();
                let started = Instant::now();
                decompress_file(&archive_path, &restored_path)?;
                decompress_time += started.elapsed();
                ensure!(
                    hash_file(&restored_path)? == source_hash,
                    "benchmark round-trip comparison failed"
                );
                final_stats = Some(stats);
            }
            let stats = final_stats.unwrap();
            let average_compress = compress_time.div_f64(iterations as f64);
            let average_decompress = decompress_time.div_f64(iterations as f64);
            println!("chunks      : {}", stats.chunk_count);
            println!(
                "ratio       : {:.1}% ({})",
                stats.ratio() * 100.0,
                human_bytes(stats.archive_size)
            );
            println!(
                "compress avg: {:.3}s, {}",
                average_compress.as_secs_f64(),
                human_rate(source_size, average_compress)
            );
            println!(
                "decode avg  : {:.3}s, {}",
                average_decompress.as_secs_f64(),
                human_rate(source_size, average_decompress)
            );
            println!("round-trip  : OK");
        }
    }
    Ok(())
}

fn read_password(file: Option<&Path>, confirm: bool) -> Result<Zeroizing<Vec<u8>>> {
    let password = if let Some(path) = file {
        let mut bytes = Zeroizing::new(Vec::new());
        File::open(path)
            .context("could not open password file")?
            .take(1029)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 1028, "password file is too large");
        if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            bytes.drain(..3);
        }
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        std::str::from_utf8(&bytes).context("password file must be UTF-8")?;
        ensure!(
            !bytes.contains(&b'\n') && !bytes.contains(&b'\r'),
            "password file must contain one line"
        );
        bytes
    } else {
        let first = Zeroizing::new(rpassword::prompt_password("パスワード / Password: ")?);
        if confirm {
            let second = Zeroizing::new(rpassword::prompt_password("確認 / Confirm password: ")?);
            ensure!(
                *first == *second,
                "パスワードが一致しません / passwords do not match"
            );
        }
        Zeroizing::new(first.as_bytes().to_vec())
    };
    ensure!(
        !password.is_empty() && password.len() <= 1024,
        "password must be 1-1024 UTF-8 bytes"
    );
    Ok(password)
}

fn zip_limits(max_entries: Option<usize>, max_output_bytes: Option<u64>) -> ZipLimits {
    let defaults = ZipLimits::default();
    ZipLimits {
        max_entries: max_entries.unwrap_or(defaults.max_entries),
        max_output_bytes: max_output_bytes.unwrap_or(defaults.max_output_bytes),
    }
}

fn prepare_output(input: &Path, path: &Path, force: bool) -> Result<()> {
    validate_distinct(input, path)?;
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to replace it)",
            path.display()
        );
    }
    Ok(())
}

fn validate_distinct(input: &Path, output: &Path) -> Result<()> {
    if input == output {
        bail!("input and output paths must be different");
    }
    if input.exists() && output.exists() {
        let input = fs::canonicalize(input)?;
        let output = fs::canonicalize(output)?;
        ensure!(input != output, "input and output resolve to the same file");
    }
    Ok(())
}

fn default_compressed_path(input: &Path) -> PathBuf {
    let mut output = input.as_os_str().to_owned();
    output.push(".fst");
    PathBuf::from(output)
}

fn default_decompressed_path(input: &Path) -> PathBuf {
    let base = if input
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fst"))
    {
        input.with_extension("")
    } else {
        input.to_owned()
    };
    unique_output_path(&base, true)
}

fn default_zip_directory(input: &Path) -> PathBuf {
    unique_output_path(&input.with_extension(""), false)
}

fn default_directory_output(input: &Path) -> PathBuf {
    unique_output_path(&input.with_extension(""), false)
}

fn unique_output_path(base: &Path, preserve_extension: bool) -> PathBuf {
    if !base.exists() {
        return base.to_owned();
    }
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let stem = if preserve_extension {
        base.file_stem().unwrap_or(base.as_os_str())
    } else {
        base.file_name().unwrap_or(base.as_os_str())
    };
    for suffix in 2.. {
        let marker = format!(" ({suffix})");
        let mut name = stem.to_owned();
        name.push(marker);
        let mut candidate = parent.join(name);
        if preserve_extension && let Some(extension) = base.extension() {
            candidate.set_extension(extension);
        }
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CliArchiveKind {
    File,
    Directory,
    Zip,
    Encrypted,
}

fn archive_kind(input: &Path) -> Result<CliArchiveKind> {
    let mut file = File::open(input)?;
    let mut magic = [0u8; 8];
    let read = file.read(&mut magic)?;
    if read == magic.len() && &magic == ENCRYPTED_MAGIC {
        return Ok(CliArchiveKind::Encrypted);
    }
    if read == magic.len() && &magic == DIRECTORY_MAGIC {
        return Ok(CliArchiveKind::Directory);
    }
    if read >= 4
        && matches!(
            &magic[..4],
            [b'P', b'K', 3, 4] | [b'P', b'K', 5, 6] | [b'P', b'K', 7, 8]
        )
    {
        return Ok(CliArchiveKind::Zip);
    }
    Ok(CliArchiveKind::File)
}

fn hash_file(path: &Path) -> Result<blake3::Hash> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 8 * 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn human_rate(bytes: u64, elapsed: Duration) -> String {
    if elapsed.is_zero() {
        return "measuring".to_owned();
    }
    let bytes_per_second = bytes as f64 / elapsed.as_secs_f64();
    let megabytes = bytes_per_second / 1_000_000.0;
    let gigabytes = bytes_per_second / 1_000_000_000.0;
    let megabits = bytes_per_second * 8.0 / 1_000_000.0;
    let gigabits = bytes_per_second * 8.0 / 1_000_000_000.0;
    format!("{megabytes:.2} MB/s | {gigabytes:.3} GB/s | {megabits:.1} Mbps | {gigabits:.3} Gbps")
}
