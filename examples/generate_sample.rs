//! Generate deterministic mixed-pattern data for benchmarks.

use std::{
    env,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("fastener-benchmark.bin"));
    let mebibytes = env::args()
        .nth(2)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(16);
    let target = mebibytes * 1024 * 1024;
    let file = File::create(&output)?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, file);
    let mut written = 0usize;
    let mut state = 0x1234_5678u32;

    while written < target {
        let region = written / (1024 * 1024);
        let remaining = target - written;
        let mut block = Vec::with_capacity(4096);
        match region % 4 {
            0 => {
                while block.len() < 4096 {
                    block.extend_from_slice(b"FASTENER|telemetry|steady|0001|0001|0001\n");
                }
            }
            1 => block.extend((0..4096).map(|index| (index % 251) as u8)),
            2 => block.resize(4096, 0),
            _ => {
                for _ in 0..4096 {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    block.push(state as u8);
                }
            }
        }
        let count = remaining.min(block.len());
        writer.write_all(&block[..count])?;
        written += count;
    }
    writer.flush()?;
    println!("wrote {written} bytes to {}", output.display());
    Ok(())
}
