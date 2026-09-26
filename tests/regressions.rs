use fastener::{
    CompressOptions, ProgressPhase, compress_bytes, compress_file_with_progress,
    compress_zip_file_with_progress, decompress_file, verify_file,
};
use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
};

#[test]
fn whole_file_checksum_corruption_is_rejected_and_output_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let archive_path = temp.path().join("bad.fst");
    let output = temp.path().join("restored.bin");
    // Include the empty case: it must not bypass the whole-file checksum.
    for source in [Vec::new(), vec![42u8; 200_000]] {
        let (mut archive, _) = compress_bytes(&source, &CompressOptions::default()).unwrap();
        archive[28] ^= 1;
        fs::write(&archive_path, archive).unwrap();
        assert!(
            verify_file(&archive_path)
                .unwrap_err()
                .to_string()
                .contains("whole-file")
        );
        fs::write(&output, b"keep existing output").unwrap();
        assert!(decompress_file(&archive_path, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"keep existing output");
        fs::remove_file(&output).unwrap();
        assert!(decompress_file(&archive_path, &output).is_err());
        assert!(!output.exists());
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn damaged_chunk_does_not_destroy_existing_output() {
    let temp = tempfile::tempdir().unwrap();
    let archive_path = temp.path().join("bad.fst");
    let output = temp.path().join("existing.bin");
    let (mut archive, _) = compress_bytes(&vec![7; 200_000], &CompressOptions::default()).unwrap();
    archive[80] ^= 1; // First chunk's checksum, leaving the container parseable.
    fs::write(&archive_path, archive).unwrap();
    fs::write(&output, b"keep").unwrap();
    assert!(decompress_file(&archive_path, &output).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"keep");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
}

#[test]
fn interrupted_compression_preserves_destination_and_cleans_temporary_files() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.bin");
    let output = temp.path().join("existing.archive");
    fs::write(&input, vec![11; 100_000]).unwrap();
    for zip in [false, true] {
        fs::write(&output, b"keep").unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| {
            if zip {
                compress_zip_file_with_progress(&input, &output, 1, |info| {
                    if info.completed > 0 {
                        panic!("simulated interruption");
                    }
                })
                .unwrap();
            } else {
                compress_file_with_progress(&input, &output, &CompressOptions::default(), |info| {
                    if info.phase == ProgressPhase::Compressing {
                        panic!("simulated interruption");
                    }
                })
                .unwrap();
            }
        }));
        assert!(result.is_err());
        assert_eq!(fs::read(&output).unwrap(), b"keep");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }
}

#[test]
fn in_memory_compression_rejects_invalid_levels() {
    for level in [-1, 23] {
        let options = CompressOptions {
            compression_level: level,
            ..Default::default()
        };
        assert!(compress_bytes(b"input", &options).is_err());
    }
}

#[test]
fn cli_force_keeps_output_on_checksum_failure() {
    let temp = tempfile::tempdir().unwrap();
    let archive_path = temp.path().join("bad.fst");
    let output = temp.path().join("existing.bin");
    let (mut archive, _) = compress_bytes(b"hello", &CompressOptions::default()).unwrap();
    archive[28] ^= 1;
    fs::write(&archive_path, archive).unwrap();
    fs::write(&output, b"keep").unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("decompress")
        .arg(&archive_path)
        .arg("-o")
        .arg(&output)
        .arg("--force")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&output).unwrap(), b"keep");
}
