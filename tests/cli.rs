use std::{fs, process::Command};

#[test]
fn cli_does_not_expose_boundary_analysis() {
    let output = Command::new(env!("CARGO_BIN_EXE_fastener"))
        .args(["compress", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("--analyzer"));
    assert!(!help.contains("--gpu"));

    let root = Command::new(env!("CARGO_BIN_EXE_fastener"))
        .arg("--help")
        .output()
        .unwrap();
    let root_help = String::from_utf8_lossy(&root.stdout).to_lowercase();
    assert!(!root_help.contains("boundary"));
    assert!(!root_help.contains("gpu"));
}

#[test]
fn cli_compress_verify_decompress_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.bin");
    let archive = temp.path().join("input.fst");
    let output = temp.path().join("output.bin");
    let source: Vec<u8> = (0..400_000).map(|i| ((i / 97) % 251) as u8).collect();
    fs::write(&input, &source).unwrap();

    let binary = env!("CARGO_BIN_EXE_fastener");
    let compressed = Command::new(binary)
        .args([
            "compress",
            input.to_str().unwrap(),
            "-o",
            archive.to_str().unwrap(),
            "--chunk-size",
            "65536",
        ])
        .output()
        .unwrap();
    assert!(
        compressed.status.success(),
        "{}",
        String::from_utf8_lossy(&compressed.stderr)
    );

    assert!(
        Command::new(binary)
            .args(["verify", archive.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(binary)
            .args([
                "decompress",
                archive.to_str().unwrap(),
                "-o",
                output.to_str().unwrap()
            ])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(output).unwrap(), source);
}

#[test]
fn cli_directory_is_one_archive_and_restores_tree() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("folder");
    let nested = input.join("nested");
    let empty = input.join("empty");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(&empty).unwrap();
    fs::write(input.join("one.txt"), b"one").unwrap();
    fs::write(nested.join("two.txt"), b"two").unwrap();
    let archive = temp.path().join("folder.fst");
    let output = temp.path().join("restored");
    let binary = env!("CARGO_BIN_EXE_fastener");

    assert!(
        Command::new(binary)
            .args([
                "compress",
                input.to_str().unwrap(),
                "-o",
                archive.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(archive.is_file());
    assert!(
        Command::new(binary)
            .args(["verify", archive.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(binary)
            .args([
                "decompress",
                archive.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(output.join("one.txt")).unwrap(), b"one");
    assert_eq!(fs::read(output.join("nested/two.txt")).unwrap(), b"two");
    assert!(output.join("empty").is_dir());
}

#[test]
fn cli_zip_compress_is_standard_zip_and_round_trips() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.bin");
    let archive = temp.path().join("input.zip");
    let output = temp.path().join("unzipped");
    let source: Vec<u8> = (0..500_000).map(|i| ((i / 31) % 251) as u8).collect();
    fs::write(&input, &source).unwrap();
    let binary = env!("CARGO_BIN_EXE_fastener");

    assert!(
        Command::new(binary)
            .args([
                "zip-compress",
                input.to_str().unwrap(),
                "-o",
                archive.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(binary)
            .args([
                "decompress",
                archive.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(output.join("input.bin")).unwrap(), source);
}

#[test]
fn cli_default_decompress_uses_numbered_collision_name() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.bin");
    let archive = temp.path().join("input.bin.fst");
    let numbered_output = temp.path().join("input (2).bin");
    let source = b"numbered collision output";
    fs::write(&input, source).unwrap();
    let binary = env!("CARGO_BIN_EXE_fastener");

    assert!(
        Command::new(binary)
            .args([
                "compress",
                input.to_str().unwrap(),
                "-o",
                archive.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(binary)
            .args(["decompress", archive.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(input).unwrap(), source);
    assert_eq!(fs::read(numbered_output).unwrap(), source);
}
