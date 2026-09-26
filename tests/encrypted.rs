use fastener::{
    CompressOptions, ENCRYPTED_MAGIC, compress_encrypted_with_progress as compress,
    decompress_encrypted_with_progress as decompress, encrypted_is_directory,
    verify_encrypted_with_progress as verify,
};
use std::{fs, path::Path, process::Command};

const PASSWORD: &[u8] = b"test-only long password 2026";

#[test]
fn interrupted_encryption_preserves_existing_archive_and_cleans_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let archive = temp.path().join("archive.fst");
    fs::write(&input, vec![5u8; 200_000]).unwrap();
    fs::write(&archive, b"keep original archive").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = compress(&input, &archive, &options(0), PASSWORD, |info| {
            if info.completed > 0 {
                panic!("simulated interruption");
            }
        });
    }));
    assert!(result.is_err());
    assert_eq!(fs::read(&archive).unwrap(), b"keep original archive");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
}

fn options(level: i32) -> CompressOptions {
    CompressOptions {
        compression_level: level,
        target_chunk_size: 64 * 1024,
    }
}

fn random_data(len: usize) -> Vec<u8> {
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

#[test]
fn encrypted_file_round_trips_in_all_modes_and_hides_names() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("非公開の研究資料.txt");
    let archive = temp.path().join("data.fst");
    let output = temp.path().join("restored");
    let mut data = vec![b'A'; 1_000_000];
    data.extend(random_data(500_000));
    fs::write(&input, &data).unwrap();
    for level in [0, 1, 12] {
        let report = compress(&input, &archive, &options(level), PASSWORD, |_| {}).unwrap();
        assert!(report.chunks > 8);
        assert!(!encrypted_is_directory(&archive).unwrap());
        assert_eq!(
            verify(&archive, PASSWORD, |_| {}).unwrap().original_size,
            data.len() as u64
        );
        fs::write(&output, b"old output").unwrap();
        decompress(&archive, &output, PASSWORD, |_| {}).unwrap();
        assert_eq!(fs::read(&output).unwrap(), data);
        let bytes = fs::read(&archive).unwrap();
        assert_eq!(&bytes[..8], ENCRYPTED_MAGIC);
        let name = "非公開の研究資料.txt".as_bytes();
        assert!(!bytes.windows(name.len()).any(|w| w == name));
        assert!(!bytes.windows(PASSWORD.len()).any(|w| w == PASSWORD));
    }
}

#[test]
fn encrypted_empty_file_and_empty_directory_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    for directory in [false, true] {
        let input = temp.path().join(if directory { "folder" } else { "file" });
        let output = temp
            .path()
            .join(if directory { "folder-out" } else { "file-out" });
        let archive = temp.path().join("archive.fst");
        if directory {
            fs::create_dir(&input).unwrap();
        } else {
            fs::write(&input, []).unwrap();
        }
        compress(&input, &archive, &options(1), PASSWORD, |_| {}).unwrap();
        assert_eq!(verify(&archive, PASSWORD, |_| {}).unwrap().original_size, 0);
        decompress(&archive, &output, PASSWORD, |_| {}).unwrap();
        if directory {
            assert_eq!(fs::read_dir(output).unwrap().count(), 0);
        } else {
            assert!(fs::read(output).unwrap().is_empty());
        }
    }
}

#[test]
fn directory_round_trip_is_atomic_on_authentication_failure() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source");
    fs::create_dir_all(input.join("資料/空フォルダー")).unwrap();
    fs::write(input.join("資料/秘密.txt"), "日本語の内容").unwrap();
    fs::write(input.join("empty"), []).unwrap();
    let archive = temp.path().join("archive.fst");
    let output = temp.path().join("restored");
    compress(&input, &archive, &options(0), PASSWORD, |_| {}).unwrap();
    let good = fs::read(&archive).unwrap();
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    fs::write(&archive, bad).unwrap();
    assert!(decompress(&archive, &output, PASSWORD, |_| {}).is_err());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    fs::write(&archive, good).unwrap();
    let report = decompress(&archive, &output, PASSWORD, |_| {}).unwrap();
    assert!(report.is_directory);
    assert_eq!(report.files, 2);
    assert_eq!(
        fs::read_to_string(output.join("資料/秘密.txt")).unwrap(),
        "日本語の内容"
    );
    assert!(output.join("資料/空フォルダー").is_dir());
    assert!(decompress(&archive, &output, PASSWORD, |_| {}).is_err());
}

fn frame_ranges(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 48;
    while cursor < bytes.len() {
        let size = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        ranges.push(cursor..cursor + 4 + size);
        cursor += 4 + size;
    }
    ranges
}

fn rejected_preserving_output(archive: &Path, output: &Path, bytes: &[u8], password: &[u8]) {
    fs::write(archive, bytes).unwrap();
    fs::write(output, b"keep existing data").unwrap();
    assert!(verify(archive, password, |_| {}).is_err());
    assert!(decompress(archive, output, password, |_| {}).is_err());
    assert_eq!(fs::read(output).unwrap(), b"keep existing data");
}

#[test]
fn password_tampering_truncation_reordering_and_replay_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source");
    let archive = temp.path().join("archive.fst");
    let output = temp.path().join("output");
    fs::write(&input, random_data(700_000)).unwrap();
    compress(&input, &archive, &options(0), PASSWORD, |_| {}).unwrap();
    let good = fs::read(&archive).unwrap();
    let frames = frame_ranges(&good);
    assert!(frames.len() > 10);
    rejected_preserving_output(&archive, &output, &good, b"wrong password");
    for offset in [
        8,
        10,
        11,
        16,
        32,
        48,
        frames[0].end - 1,
        frames[1].start + 5,
        good.len() - 1,
    ] {
        let mut corrupt = good.clone();
        corrupt[offset] ^= 1;
        rejected_preserving_output(&archive, &output, &corrupt, PASSWORD);
    }
    for len in [
        0,
        47,
        frames[0].end,
        frames[1].end,
        frames.last().unwrap().start,
        good.len() - 1,
    ] {
        rejected_preserving_output(&archive, &output, &good[..len], PASSWORD);
    }
    let mut reordered = good[..frames[1].start].to_vec();
    reordered.extend_from_slice(&good[frames[2].clone()]);
    reordered.extend_from_slice(&good[frames[1].clone()]);
    reordered.extend_from_slice(&good[frames[2].end..]);
    rejected_preserving_output(&archive, &output, &reordered, PASSWORD);
    let mut missing = good[..frames[1].start].to_vec();
    missing.extend_from_slice(&good[frames[1].end..]);
    rejected_preserving_output(&archive, &output, &missing, PASSWORD);
    let mut extra = good.clone();
    extra.push(0);
    rejected_preserving_output(&archive, &output, &extra, PASSWORD);
    compress(&input, &archive, &options(0), PASSWORD, |_| {}).unwrap();
    let fresh = fs::read(&archive).unwrap();
    assert_ne!(&good[16..48], &fresh[16..48]);
    let fresh_frames = frame_ranges(&fresh);
    let mut replay = fresh[..fresh_frames[1].start].to_vec();
    replay.extend_from_slice(&good[frames[1].clone()]);
    replay.extend_from_slice(&fresh[fresh_frames[1].end..]);
    rejected_preserving_output(&archive, &output, &replay, PASSWORD);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
}

#[test]
fn encrypted_input_crosses_segment_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("large");
    let archive = temp.path().join("large.fst");
    let output = temp.path().join("restored");
    let data: Vec<_> = (0..64 * 1024 * 1024 + 12345)
        .map(|i| (i % 251) as u8)
        .collect();
    fs::write(&input, &data).unwrap();
    compress(
        &input,
        &archive,
        &CompressOptions::default(),
        PASSWORD,
        |_| {},
    )
    .unwrap();
    decompress(&archive, &output, PASSWORD, |_| {}).unwrap();
    assert_eq!(
        blake3::hash(&fs::read(output).unwrap()),
        blake3::hash(&data)
    );
}

#[test]
fn cli_encrypted_workflow_and_password_failure() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("source");
    let archive = temp.path().join("archive.fst");
    let output = temp.path().join("output");
    let password_file = temp.path().join("password.txt");
    fs::write(&input, b"secret data").unwrap();
    fs::write(&password_file, "検証用の長いパスワード\r\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_fastener");
    let result = Command::new(binary)
        .arg("compress")
        .arg(&input)
        .arg("-o")
        .arg(&archive)
        .arg("--encrypt")
        .arg("--password-file")
        .arg(&password_file)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        Command::new(binary)
            .arg("verify")
            .arg(&archive)
            .arg("--password-file")
            .arg(&password_file)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        Command::new(binary)
            .arg("decompress")
            .arg(&archive)
            .arg("-o")
            .arg(&output)
            .arg("--password-file")
            .arg(&password_file)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&output).unwrap(), b"secret data");
    fs::write(&password_file, b"wrong").unwrap();
    let failure = Command::new(binary)
        .arg("decompress")
        .arg(&archive)
        .arg("-o")
        .arg(&output)
        .arg("--force")
        .arg("--password-file")
        .arg(&password_file)
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert_eq!(fs::read(&output).unwrap(), b"secret data");
    assert!(!String::from_utf8_lossy(&failure.stderr).contains("wrong"));
    // Supplying a password file may never silently produce an unencrypted archive.
    assert!(
        !Command::new(binary)
            .arg("compress")
            .arg(&input)
            .arg("--password-file")
            .arg(&password_file)
            .output()
            .unwrap()
            .status
            .success()
    );
}
