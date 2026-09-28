use fastener::{
    CompressOptions, FstLimits, ProgressPhase, ZipLimits, compress_bytes,
    compress_directory_bundle_with_progress, compress_file_with_progress,
    compress_zip_file_with_progress, decompress_directory_bundle_with_limits_and_progress,
    decompress_directory_bundle_with_progress, decompress_file,
    decompress_file_with_limits_and_progress, extract_zip_file,
    verify_directory_bundle_with_limits_and_progress, verify_directory_bundle_with_progress,
    verify_file, verify_file_with_limits_and_progress, verify_zip_file,
    verify_zip_file_with_limits,
};
use std::{
    fs,
    io::Write,
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

#[test]
fn damaged_directory_archive_does_not_leave_partial_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source");
    let archive = temp.path().join("source.fst");
    let output = temp.path().join("restored");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("file.txt"), b"important data").unwrap();
    compress_directory_bundle_with_progress(&input, &archive, &CompressOptions::default(), |_| {})
        .unwrap();
    let mut bytes = fs::read(&archive).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&archive, bytes).unwrap();

    assert!(decompress_directory_bundle_with_progress(&archive, &output, |_| {}).is_err());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
}

#[test]
fn invalid_zip_does_not_leave_partial_output() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("bad.zip");
    let output = temp.path().join("restored");
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("node", options).unwrap();
    writer.write_all(b"file").unwrap();
    writer.start_file("node/child", options).unwrap();
    writer.write_all(b"child").unwrap();
    writer.finish().unwrap();

    assert!(verify_zip_file(&archive).is_err());
    assert!(extract_zip_file(&archive, &output).is_err());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn forged_directory_counts_are_rejected_before_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("forged.fst");
    let output = temp.path().join("output");
    let mut bytes = b"FASTDIR1".to_vec();
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    fs::write(&archive, bytes).unwrap();
    assert!(verify_directory_bundle_with_progress(&archive, |_| {}).is_err());
    assert!(decompress_directory_bundle_with_progress(&archive, &output, |_| {}).is_err());
    assert!(!output.exists());
}

#[test]
fn directory_and_zip_reject_windows_device_names() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("reserved.fst");
    let output = temp.path().join("output");
    let mut bytes = b"FASTDIR1".to_vec();
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&[1, 0, 0, 0]);
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(b"CON");
    fs::write(&archive, bytes).unwrap();
    assert!(verify_directory_bundle_with_progress(&archive, |_| {}).is_err());
    assert!(decompress_directory_bundle_with_progress(&archive, &output, |_| {}).is_err());
    assert!(!output.exists());

    let zip_path = temp.path().join("reserved.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
    zip.start_file("CON", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"content").unwrap();
    zip.finish().unwrap();
    assert!(verify_zip_file(&zip_path).is_err());
    assert!(extract_zip_file(&zip_path, &output).is_err());
    assert!(!output.exists());
}

#[test]
fn directory_verification_rejects_file_as_parent() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("collision.fst");
    let output = temp.path().join("output");
    let (embedded, _) = compress_bytes(b"x", &CompressOptions::default()).unwrap();
    let mut bytes = b"FASTDIR1".to_vec();
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&2u64.to_le_bytes());
    for path in ["node", "node/child"] {
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&(embedded.len() as u64).to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
        bytes.extend_from_slice(&embedded);
    }
    fs::write(&archive, bytes).unwrap();
    assert!(verify_directory_bundle_with_progress(&archive, |_| {}).is_err());
    assert!(decompress_directory_bundle_with_progress(&archive, &output, |_| {}).is_err());
    assert!(!output.exists());
}

#[test]
fn oversized_fst_chunk_is_rejected_without_decoding() {
    let temp = tempfile::tempdir().unwrap();
    let archive_path = temp.path().join("large-chunk.fst");
    let (mut archive, _) = compress_bytes(b"small", &CompressOptions::default()).unwrap();
    archive[12..20].copy_from_slice(&(u32::MAX as u64).to_le_bytes());
    archive[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&archive_path, archive).unwrap();
    assert!(verify_file(&archive_path).is_err());
}

#[test]
fn fst_and_directory_expansion_limits_precede_output_creation() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.bin");
    let fst = temp.path().join("input.fst");
    let restored = temp.path().join("restored.bin");
    fs::write(&input, vec![3u8; 128 * 1024]).unwrap();
    compress_file_with_progress(&input, &fst, &CompressOptions::default(), |_| {}).unwrap();
    let small = FstLimits {
        max_output_bytes: 1024,
    };
    assert!(verify_file_with_limits_and_progress(&fst, small, |_| {}).is_err());
    assert!(decompress_file_with_limits_and_progress(&fst, &restored, small, |_| {}).is_err());
    assert!(!restored.exists());
    let large = FstLimits {
        max_output_bytes: 128 * 1024,
    };
    verify_file_with_limits_and_progress(&fst, large, |_| {}).unwrap();
    decompress_file_with_limits_and_progress(&fst, &restored, large, |_| {}).unwrap();
    assert_eq!(fs::read(&restored).unwrap(), fs::read(&input).unwrap());

    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("input.bin"), vec![3u8; 128 * 1024]).unwrap();
    let bundle = temp.path().join("bundle.fst");
    let directory = temp.path().join("directory");
    compress_directory_bundle_with_progress(&source, &bundle, &CompressOptions::default(), |_| {})
        .unwrap();
    assert!(verify_directory_bundle_with_limits_and_progress(&bundle, small, |_| {}).is_err());
    assert!(
        decompress_directory_bundle_with_limits_and_progress(&bundle, &directory, small, |_| {})
            .is_err()
    );
    assert!(!directory.exists());
    verify_directory_bundle_with_limits_and_progress(&bundle, large, |_| {}).unwrap();
    decompress_directory_bundle_with_limits_and_progress(&bundle, &directory, large, |_| {})
        .unwrap();
    assert_eq!(
        fs::read(directory.join("input.bin")).unwrap(),
        fs::read(input).unwrap()
    );

    let result = std::process::Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("verify")
        .arg(&fst)
        .args(["--fst-max-output-bytes", "1024"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("verify")
        .arg(&fst)
        .args(["--fst-max-output-bytes", "131072"])
        .output()
        .unwrap();
    assert!(result.status.success());

    let mut forged = fs::read(&fst).unwrap();
    forged[12..20].copy_from_slice(&(65u64 * 1024 * 1024 * 1024).to_le_bytes());
    fs::write(&fst, forged).unwrap();
    assert!(
        verify_file(&fst)
            .unwrap_err()
            .to_string()
            .contains("expanded size")
    );
    let output = temp.path().join("forged-output");
    assert!(decompress_file(&fst, &output).is_err());
    assert!(!output.exists());
}

#[test]
fn interrupted_directory_compression_leaves_no_archive() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), vec![1u8; 100_000]).unwrap();
    let output = temp.path().join("source.fst");
    let result = catch_unwind(AssertUnwindSafe(|| {
        compress_directory_bundle_with_progress(
            &source,
            &output,
            &CompressOptions::default(),
            |info| {
                if info.phase == ProgressPhase::Compressing {
                    panic!("simulated interruption");
                }
            },
        )
        .unwrap();
    }));
    assert!(result.is_err());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn zip_output_budget_can_be_raised_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("limited.zip");
    let output = temp.path().join("restored");
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    writer
        .start_file("data.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"1234567890").unwrap();
    writer.finish().unwrap();

    let small = ZipLimits {
        max_entries: 1,
        max_output_bytes: 9,
    };
    assert!(verify_zip_file_with_limits(&archive, small).is_err());
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("verify")
        .arg(&archive)
        .args(["--zip-max-output-bytes", "9"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("decompress")
        .arg(&archive)
        .arg("-o")
        .arg(&output)
        .args(["--zip-max-output-bytes", "9"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());

    let large = ZipLimits {
        max_output_bytes: 10,
        ..small
    };
    assert!(verify_zip_file_with_limits(&archive, large).is_ok());
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("verify")
        .arg(&archive)
        .args(["--zip-max-output-bytes", "10"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("decompress")
        .arg(&archive)
        .arg("-o")
        .arg(&output)
        .args(["--zip-max-output-bytes", "10"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(output.join("data.txt")).unwrap(), b"1234567890");
}
