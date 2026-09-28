use fastener::*;
use std::{fs, path::Path, process::Command};

fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x12345678u32;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

fn fixture(root: &Path, len: usize) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
    let input = root.join("archive.fst");
    let parity = recovery_path(&input);
    let (archive, _) = compress_bytes(&noise(len), &CompressOptions::default()).unwrap();
    fs::write(&input, &archive).unwrap();
    let info = create_recovery_with_progress(&input, &parity, |_| {}).unwrap();
    assert_eq!(fs::metadata(&parity).unwrap().len(), info.recovery_bytes);
    (input, parity, archive)
}

#[test]
fn two_unknown_corrupt_shards_recover_exactly_and_keep_damaged_input() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, original) = fixture(dir.path(), 300_000);
    let info = recovery_info(&parity).unwrap();
    let mut bad = original.clone();
    bad[0] ^= 0x80;
    bad[info.shard_bytes + 53..info.shard_bytes + 100].fill(0);
    fs::write(&input, &bad).unwrap();
    let output = repaired_path(&input);
    let report = repair_with_progress(&input, &parity, &output, None, |_| {}).unwrap();
    assert_eq!(report.repaired_shards, 2);
    assert_eq!(fs::read(&output).unwrap(), original);
    assert_eq!(fs::read(&input).unwrap(), bad);
    assert!(repair_with_progress(&input, &parity, &input, None, |_| {}).is_err());
    assert!(repair_with_progress(&input, &parity, &parity, None, |_| {}).is_err());
}

#[test]
fn fst_repair_respects_expanded_size_limit_before_publication() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, original) = fixture(dir.path(), 128 * 1024);
    let output = repaired_path(&input);
    let small = FstLimits {
        max_output_bytes: 1024,
    };
    assert!(
        repair_with_all_limits_and_progress(
            &input,
            &parity,
            &output,
            None,
            RecoveryLimits {
                fst: small,
                ..Default::default()
            },
            |_| {},
        )
        .is_err()
    );
    assert!(!output.exists());
    repair_with_all_limits_and_progress(
        &input,
        &parity,
        &output,
        None,
        RecoveryLimits {
            fst: FstLimits {
                max_output_bytes: 128 * 1024,
            },
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(fs::read(output).unwrap(), original);
}

#[test]
fn over_capacity_or_deleted_middle_bytes_fail_without_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, original) = fixture(dir.path(), 300_000);
    let info = recovery_info(&parity).unwrap();
    let output = repaired_path(&input);
    fs::write(&output, b"existing output").unwrap();
    let mut bad = original.clone();
    for i in 0..3 {
        bad[i * info.shard_bytes + 3] ^= 1;
    }
    fs::write(&input, &bad).unwrap();
    assert!(repair_with_progress(&input, &parity, &output, None, |_| {}).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"existing output");
    let mut deleted = original;
    deleted.drain(100..200);
    fs::write(&input, deleted).unwrap();
    assert!(repair_with_progress(&input, &parity, &output, None, |_| {}).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"existing output");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[test]
fn redundant_headers_indexes_and_parity_damage() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, original) = fixture(dir.path(), 300_000);
    let info = recovery_info(&parity).unwrap();
    let original_parity = fs::read(&parity).unwrap();
    let output = repaired_path(&input);
    let index_len = 744;
    let second_index = 128 + index_len + 2 * info.shard_bytes;
    let mut bad = original.clone();
    bad[1] ^= 1;
    fs::write(&input, bad).unwrap();
    for positions in [
        vec![0, 128],
        vec![original_parity.len() - 1, second_index],
        vec![0, 128, 128 + index_len + 3],
    ] {
        let mut damaged = original_parity.clone();
        for pos in positions {
            damaged[pos] ^= 1;
        }
        fs::write(&parity, damaged).unwrap();
        repair_with_progress(&input, &parity, &output, None, |_| {}).unwrap();
        assert_eq!(fs::read(&output).unwrap(), original);
    }
    for positions in [
        vec![0, original_parity.len() - 1],
        vec![128, second_index],
        vec![128 + index_len, 128 + index_len + info.shard_bytes],
    ] {
        let mut damaged = original_parity.clone();
        for pos in positions {
            damaged[pos] ^= 1;
        }
        fs::write(&parity, damaged).unwrap();
        assert!(repair_with_progress(&input, &parity, &output, None, |_| {}).is_err());
        assert_eq!(fs::read(&output).unwrap(), original);
    }
    // With the archive intact, both lost parity shards need no reconstruction.
    fs::write(&input, &original).unwrap();
    repair_with_progress(&input, &parity, &output, None, |_| {}).unwrap();
    let mut conflicting = original_parity.clone();
    conflicting[32] ^= 1;
    let hash = blake3::hash(&conflicting[..96]);
    conflicting[96..128].copy_from_slice(hash.as_bytes());
    fs::write(&parity, conflicting).unwrap();
    assert!(recovery_info(&parity).is_err());
    let mut conflicting = original_parity;
    conflicting[128 + 8] ^= 1;
    let hash = blake3::hash(&conflicting[128..128 + index_len - 32]);
    conflicting[128 + index_len - 32..128 + index_len].copy_from_slice(hash.as_bytes());
    fs::write(&parity, conflicting).unwrap();
    assert!(repair_with_progress(&input, &parity, &output, None, |_| {}).is_err());
    assert_eq!(fs::read(&output).unwrap(), original);
}

#[test]
fn partial_last_shard_truncation_and_multiple_groups() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, original) = fixture(dir.path(), 20 * 1024 * 1024 + 32_768);
    let info = recovery_info(&parity).unwrap();
    assert_eq!(info.groups, 2);
    let mut bad = original.clone();
    bad[0] ^= 1;
    bad[info.shard_bytes + 10] ^= 2;
    bad.truncate(bad.len() - 10_000);
    fs::write(&input, bad).unwrap();
    let output = repaired_path(&input);
    repair_with_progress(&input, &parity, &output, None, |_| {}).unwrap();
    assert_eq!(fs::read(output).unwrap(), original);
}

#[test]
fn tiny_empty_content_archives_and_missing_sidecar_tail() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, original) = fixture(dir.path(), 0);
    assert_eq!(recovery_info(&parity).unwrap().shard_bytes, 4096);
    fs::write(&input, []).unwrap();
    // Keep first index and one parity shard; lose second parity, index and footer.
    let bytes = fs::read(&parity).unwrap();
    fs::write(&parity, &bytes[..128 + 744 + 4096]).unwrap();
    let output = repaired_path(&input);
    repair_with_progress(&input, &parity, &output, None, |_| {}).unwrap();
    assert_eq!(fs::read(output).unwrap(), original);
}

#[test]
fn encrypted_repair_requires_authentication_before_publication() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("秘密.txt");
    fs::write(&source, noise(200_000)).unwrap();
    let archive = dir.path().join("encrypted.fst");
    compress_encrypted_with_progress(
        &source,
        &archive,
        &CompressOptions::default(),
        b"correct",
        |_| {},
    )
    .unwrap();
    let parity = recovery_path(&archive);
    create_recovery_with_progress(&archive, &parity, |_| {}).unwrap();
    assert!(recovery_info(&parity).unwrap().encrypted);
    let original = fs::read(&archive).unwrap();
    let mut broken = original.clone();
    broken[0] ^= 1;
    fs::write(&archive, broken).unwrap();
    let output = repaired_path(&archive);
    fs::write(&output, b"keep").unwrap();
    for password in [None, Some(b"incorrect".as_slice())] {
        assert!(repair_with_progress(&archive, &parity, &output, password, |_| {}).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"keep");
    }
    repair_with_progress(&archive, &parity, &output, Some(b"correct"), |_| {}).unwrap();
    assert_eq!(fs::read(&output).unwrap(), original);
    verify_encrypted_with_progress(&output, b"correct", |_| {}).unwrap();
    let secret = dir.path().join("password.txt");
    fs::write(&secret, b"correct\n").unwrap();
    let cli = env!("CARGO_BIN_EXE_fastener");
    assert!(
        Command::new(cli)
            .arg("repair")
            .arg(&archive)
            .arg("--password-file")
            .arg(&secret)
            .arg("--force")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&output).unwrap(), original);
    fs::write(&secret, b"wrong\n").unwrap();
    assert!(
        !Command::new(cli)
            .arg("repair")
            .arg(&archive)
            .arg("--password-file")
            .arg(&secret)
            .arg("--force")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&output).unwrap(), original);
}

#[test]
fn zip_and_directory_bundle_recovery_verify_formats() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    let input = folder.join("data");
    fs::write(&input, noise(8000)).unwrap();
    for is_zip in [true, false] {
        let archive = dir
            .path()
            .join(if is_zip { "archive.zip" } else { "bundle.fst" });
        if is_zip {
            compress_zip_file(&input, &archive, 1).unwrap();
        } else {
            compress_directory_bundle_with_progress(
                &folder,
                &archive,
                &CompressOptions::default(),
                |_| {},
            )
            .unwrap();
        }
        let original = fs::read(&archive).unwrap();
        let parity = recovery_path(&archive);
        create_recovery_with_progress(&archive, &parity, |_| {}).unwrap();
        let mut broken = original.clone();
        broken[0] ^= 1;
        fs::write(&archive, broken).unwrap();
        let output = repaired_path(&archive);
        if is_zip {
            let small = ZipLimits {
                max_entries: 1,
                max_output_bytes: 7999,
            };
            assert!(
                repair_with_zip_limits_and_progress(
                    &archive,
                    &parity,
                    &output,
                    None,
                    small,
                    |_| {},
                )
                .is_err()
            );
            assert!(!output.exists());
            repair_with_zip_limits_and_progress(
                &archive,
                &parity,
                &output,
                None,
                ZipLimits {
                    max_output_bytes: 8000,
                    ..small
                },
                |_| {},
            )
            .unwrap();
        } else {
            repair_with_progress(&archive, &parity, &output, None, |_| {}).unwrap();
        }
        assert_eq!(fs::read(output).unwrap(), original);
    }
}

#[test]
fn invalid_archives_or_sidecars_and_interruption_never_replace_output() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, mut original) = fixture(dir.path(), 10_000);
    let before_parity = fs::read(&parity).unwrap();
    let interrupted = std::panic::catch_unwind(|| {
        create_recovery_with_progress(&input, &parity, |info| {
            if info.completed > 0 {
                panic!("cancel");
            }
        })
        .unwrap();
    });
    assert!(interrupted.is_err());
    assert_eq!(fs::read(&parity).unwrap(), before_parity);
    let output = repaired_path(&input);
    fs::write(&output, b"keep").unwrap();
    let interrupted = std::panic::catch_unwind(|| {
        repair_with_progress(&input, &parity, &output, None, |info| {
            if info.completed > 0 {
                panic!("cancel");
            }
        })
        .unwrap();
    });
    assert!(interrupted.is_err());
    assert_eq!(fs::read(&output).unwrap(), b"keep");
    // A parity file made from an already-invalid archive cannot bypass format verification.
    original[28] ^= 1;
    fs::write(&input, original).unwrap();
    create_recovery_with_progress(&input, &parity, |_| {}).unwrap();
    assert!(repair_with_progress(&input, &parity, &output, None, |_| {}).is_err());
    fs::write(&parity, b"FSTPAR01").unwrap();
    assert!(recovery_info(&parity).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"keep");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[test]
fn cli_create_estimate_repair_and_overwrite_rules() {
    let dir = tempfile::tempdir().unwrap();
    let (input, parity, original) = fixture(dir.path(), 100_000);
    fs::remove_file(&parity).unwrap();
    let cli = env!("CARGO_BIN_EXE_fastener");
    let result = Command::new(cli)
        .args(["recovery-create", input.to_str().unwrap(), "--estimate"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(!parity.exists());
    assert!(
        Command::new(cli)
            .args(["recovery-create", input.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !Command::new(cli)
            .args(["recovery-create", input.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut bad = original.clone();
    bad[0] ^= 1;
    fs::write(&input, bad).unwrap();
    assert!(
        Command::new(cli)
            .args(["repair", input.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(repaired_path(&input)).unwrap(), original);
    assert!(
        !Command::new(cli)
            .args(["repair", input.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn forged_zip_kind_cannot_bypass_encrypted_archive_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("data");
    fs::write(&source, b"zip payload").unwrap();
    let archive = dir.path().join("prefixed.zip");
    compress_zip_file(&source, &archive, 1).unwrap();
    let mut prefixed = ENCRYPTED_MAGIC.to_vec();
    prefixed.extend(fs::read(&archive).unwrap());
    fs::write(&archive, prefixed).unwrap();
    // ZIP readers permit executable prefixes. An encrypted magic must still
    // force encrypted validation, even when untrusted metadata claims ZIP.
    verify_zip_file(&archive).unwrap();
    let parity = recovery_path(&archive);
    create_recovery_with_progress(&archive, &parity, |_| {}).unwrap();
    let mut bytes = fs::read(&parity).unwrap();
    for offset in [0, bytes.len() - 128] {
        bytes[offset + 64] = 2;
        let hash = blake3::hash(&bytes[offset..offset + 96]);
        bytes[offset + 96..offset + 128].copy_from_slice(hash.as_bytes());
    }
    fs::write(&parity, bytes).unwrap();
    let output = repaired_path(&archive);
    let error = repair_with_progress(&archive, &parity, &output, None, |_| {}).unwrap_err();
    assert!(error.to_string().contains("kind does not match"));
    assert!(!output.exists());
}
