//! Memory-only comparison of content-aware and fixed-length boundaries.
//! Usage: compare_boundaries INPUT [ITERATIONS=5] [TARGET_BYTES=8388608]
use anyhow::{Context, Result, ensure};
use fastener::analyze;
use rayon::prelude::*;
use std::{env, fs, time::Instant};

struct Chunk {
    start: usize,
    len: usize,
    raw: bool,
    payload: Vec<u8>,
    hash: blake3::Hash,
}

fn main() -> Result<()> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(15)
        .build_global()?;
    let mut args = env::args_os().skip(1);
    let path = args.next().context("input file required")?;
    let iterations = args
        .next()
        .map(|a| a.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(5);
    let target = args
        .next()
        .map(|a| a.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(8 * 1024 * 1024);
    ensure!(
        iterations > 0 && target >= 8192,
        "invalid iterations/target"
    );
    let data = fs::read(path)?;
    let expected_hash = blake3::hash(&data);
    for iteration in 0..=iterations {
        let strategies = if iteration % 2 == 0 {
            ["fixed", "content"]
        } else {
            ["content", "fixed"]
        };
        for strategy in strategies {
            let start = Instant::now();
            let boundaries = if strategy == "content" {
                // Match file compression's 64 MiB segmentation.
                let mut all = vec![0];
                for (index, segment) in data.chunks(64 * 1024 * 1024).enumerate() {
                    all.extend(
                        analyze(segment, target, data.len())?
                            .boundaries
                            .into_iter()
                            .skip(1)
                            .map(|offset| index * 64 * 1024 * 1024 + offset),
                    );
                }
                all
            } else {
                let mut all: Vec<_> = (0..data.len()).step_by(target).collect();
                all.push(data.len());
                all
            };
            let analysis_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            let chunks = boundaries
                .par_windows(2)
                .map(|range| -> Result<_> {
                    let slice = &data[range[0]..range[1]];
                    let compressed = zstd::bulk::compress(slice, 1)?;
                    let raw = compressed.len() >= slice.len();
                    Ok(Chunk {
                        start: range[0],
                        len: slice.len(),
                        raw,
                        payload: if raw { slice.to_vec() } else { compressed },
                        hash: blake3::hash(slice),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let compression_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            let decoded = chunks
                .par_iter()
                .map(|chunk| -> Result<Vec<u8>> {
                    let bytes = if chunk.raw {
                        chunk.payload.clone()
                    } else {
                        zstd::bulk::decompress(&chunk.payload, chunk.len)?
                    };
                    ensure!(
                        bytes == data[chunk.start..chunk.start + chunk.len],
                        "restoration mismatch"
                    );
                    ensure!(blake3::hash(&bytes) == chunk.hash, "chunk hash mismatch");
                    Ok(bytes)
                })
                .collect::<Result<Vec<_>>>()?;
            let mut restored_hash = blake3::Hasher::new();
            for chunk in decoded {
                restored_hash.update_rayon(&chunk);
            }
            ensure!(
                restored_hash.finalize() == expected_hash,
                "whole-file restoration mismatch"
            );
            let decode_ms = start.elapsed().as_secs_f64() * 1000.0;
            let estimated_bytes = 60 + chunks.iter().map(|c| 52 + c.payload.len()).sum::<usize>();
            println!(
                "{{\"strategy\":\"{strategy}\",\"iteration\":{iteration},\"input_bytes\":{},\"target_bytes\":{target},\"chunks\":{},\"analysis_ms\":{analysis_ms:.6},\"compression_ms\":{compression_ms:.6},\"decode_check_ms\":{decode_ms:.6},\"estimated_fst_bytes\":{estimated_bytes},\"restored_blake3\":\"{expected_hash}\"}}",
                data.len(),
                chunks.len()
            );
        }
    }
    Ok(())
}
