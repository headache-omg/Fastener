//! Warm engine timing, excluding process startup and fixture/hash comparisons.
//! recovery_benchmark ARCHIVE OUTPUT_DIRECTORY [ITERATIONS=5]
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

fn hash(path: &Path) -> Result<blake3::Hash> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut hasher = blake3::Hasher::new();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize())
}

fn main() -> Result<()> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(15)
        .build_global()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() >= 2, "archive and output directory required");
    let input = Path::new(&args[0]);
    let work = tempfile::tempdir_in(&args[1])?;
    let iterations: usize = args
        .get(2)
        .map(|n| n.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(5);
    ensure!(iterations > 0, "iterations must be positive");
    let expected = hash(input)?;
    let damaged = work.path().join("damaged.fst");
    fs::copy(input, &damaged)?;
    let size = fs::metadata(input)?.len();
    let plan = fastener::recovery_plan(size)?;
    let mut file = File::options().read(true).write(true).open(&damaged)?;
    for offset in [0, plan.shard_bytes as u64 + 31] {
        if offset < size {
            file.seek(SeekFrom::Start(offset))?;
            let mut byte = [0];
            file.read_exact(&mut byte)?;
            byte[0] ^= 128;
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(&byte)?;
        }
    }
    drop(file);
    for iteration in 0..=iterations {
        let parity = work.path().join(format!("{iteration}.par"));
        let output = work.path().join(format!("{iteration}.fst"));
        let started = Instant::now();
        fastener::create_recovery_with_progress(input, &parity, |_| {})?;
        let create_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        fastener::repair_with_progress(&damaged, &parity, &output, None, |_| {})?;
        let repair_ms = started.elapsed().as_secs_f64() * 1000.0;
        ensure!(hash(&output)? == expected, "restored archive mismatch");
        let parity_hash = hash(&parity).context("hash recovery file")?;
        println!(
            "{{\"version\":\"{}\",\"iteration\":{},\"create_ms\":{:.4},\"repair_ms\":{:.4},\"archive_blake3\":\"{}\",\"parity_blake3\":\"{}\"}}",
            env!("CARGO_PKG_VERSION"),
            iteration,
            create_ms,
            repair_ms,
            expected,
            parity_hash
        );
    }
    Ok(())
}
