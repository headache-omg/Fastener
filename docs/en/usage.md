# Using Fastener

[日本語](../ja/usage.md) · [README](../../README.md)

The Windows GUI supports Japanese and English. Choose a file or folder, select Fast, Balanced, or Dense, and click **Compress**. Select an existing archive to decompress, verify, create recovery data, or repair. Long operations run in the background and show progress, elapsed time, throughput, and workers. The GUI worker pool uses 75% of the detected logical processors, rounded down with a minimum of one.

## CLI commands

| Command | Operation |
|---|---|
| `compress INPUT` | Create a file or folder FST, adding `.fst` to the input name by default |
| `zip-compress INPUT` | Create a single-file ZIP, replacing the input extension with `.zip` by default |
| `decompress ARCHIVE` | Extract FST or ZIP to a file or folder |
| `verify ARCHIVE` | Decode and check an archive without publishing restored files |
| `recovery-create ARCHIVE` | Create `ARCHIVE.par`; add `--estimate` to report size without writing |
| `repair ARCHIVE` | Use the matching sidecar to produce a separate archive such as `data.repaired.fst` |
| `benchmark FILE` | Time file compression and extraction, including filesystem I/O, and validate round trips |

~~~console
fastener --threads 8 compress documents --level 12
fastener compress documents --encrypt -o private.fst
fastener verify private.fst
fastener decompress private.fst -o restored-documents
fastener zip-compress report.txt -o report.zip
fastener recovery-create private.fst --estimate
fastener recovery-create private.fst
fastener repair private.fst
~~~

FST level 0 selects LZ4. Levels 1–22 select Zstandard. Balanced defaults to level 1 and Dense uses level 12. The default target chunk size is 8 MiB. The encrypted writer accepts targets from 8 KiB through 8 MiB. ZIP creation defaults to Deflate level 1 and accepts levels 0–9. A ZIP entry is decoded serially, with independent entries processed in parallel.

Without `-o`, extraction derives the name from the archive filename and adds a suffix such as ` (2)` if needed. ZIP extracts into a directory named after the archive stem. Choose a new destination for folder extraction. Where file replacement is supported, use `--force` explicitly.

The current source defaults to 64 GiB of expanded output per archive. For trusted archives, `verify`, `decompress`, and `repair` accept a format-specific byte limit: `--fst-max-output-bytes`, `--encrypted-max-output-bytes`, or `--zip-max-output-bytes`. ZIP also defaults to 100,000 entries, adjustable with `--zip-max-entries`. These limits apply during verification as well as extraction.

Keep the `.par` file from before damage occurred. Each group can recover up to two missing or damaged shards when enough valid shards remain. Damage across more than two shards in a group can prevent repair. The repair process validates the reconstructed archive, and encrypted archives also require the password. Use `--recovery PATH` when the sidecar is stored elsewhere. [Sidecar format](recovery-format.md)

## Build and package

Install Rust and a native toolchain, such as Visual Studio Build Tools for Windows MSVC. The CLI is built on Windows and Linux, and the GUI runs on Windows.

~~~console
cargo build --release --locked
cargo build --release --locked --no-default-features
~~~

The second command builds without GPU boundary analysis. On Windows run `./build-release.ps1` before `./package-oss.ps1` to produce the source and Windows ZIPs. The Windows ZIP includes the CLI and GUI under `bin/`.
