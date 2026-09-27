# Fastener 1.2.3

Fastener 1.2.3 is an MIT-licensed open-source implementation of the `.fst` archive format. It uses
content-aware chunking, CPU-parallel compression and decompression, automatic
hybrid CPU/GPU boundary analysis, and checksums. When a segment needs boundary scoring,
the CPU scores alternating samples while wgpu scores the other samples at the
same time. The partial scores are merged before boundaries are selected. Any GPU
initialization or command failure silently falls back to the full CPU scorer.

## 日本語の説明・新機能

1.2.3 writes decompressed single-file FST chunks directly into disjoint slices
of a temporary mapped output. It hashes each bounded batch while its pages are
still hot and publishes the file only after all chunk and whole-file checks pass.
See [the large-file comparison](benchmarks/v1.2.3/REPORT.md). The FST format is unchanged.

1.2.2 reuses Zstd workspaces across chunks during large FST file compression.
Raw compressed-file chunks borrow the mapped input
instead of making an extra copy. See [the large-file benchmark](benchmarks/v1.2.2/REPORT.md).
For inputs of at least 128 MiB, whole-file hashing overlaps boundary analysis.
The FST archive format is unchanged.

1.2.1 parallelizes large recovery operations and reuses bounded buffers.
See [the old/new comparison](benchmarks/v1.2.1/REPORT.md). Archive and sidecar formats are unchanged.

- [工夫点と改善](docs/工夫点と改善.md): 独自の分割・並列化と、既存Zstd/LZ4を使う部分の区別。
- [暗号化の使い方と仕様](docs/暗号化仕様.md): GUI/CLI、ファイル・フォルダーの認証付き暗号化。
- [リカバリーレコード仕様](docs/リカバリーレコード案.md): 1.2.0で外部復旧ファイルの作成・修復を実装。

1.2.0 adds optional Reed–Solomon sidecar recovery for FST, encrypted FST, folder
archives and ZIP. Use the GUI's Create recovery data / Repair buttons, or:

```powershell
fastener recovery-create archive.fst --estimate
fastener recovery-create archive.fst
fastener repair archive.fst
```

Recovery is off unless explicitly requested. Keep `archive.fst.par` with the
archive. Repair publishes a separate `.repaired.fst` only after whole-file and
archive verification; encrypted repair also requires the correct password.
Each group has 20 data and 2 parity shards. This is not a guarantee that arbitrary
10% corruption can be repaired. Middle insertions/deletions are not resynchronized.
See [the format](docs/RECOVERY_FORMAT.md) and [validation](benchmarks/v1.2.0/REPORT.md).

1.1.1 reduces GPU input transfer to sampled bytes and parallelizes CPU scoring.
See [the measured comparison](benchmarks/v1.1.1/REPORT.md).
1.1.0 added optional Argon2id + XChaCha20-Poly1305 encryption for files and folders.
The GUI has an encryption checkbox and masked password/confirmation fields.
Encrypted extraction and verification are detected automatically and require a password.
The new encrypted container has not received an independent security audit.
Inputs smaller than 16 MiB now use only CPU scoring even with small target chunks.

## Windows GUI

The release package includes `fastener-gui.exe`. It provides file and folder
browsing and Compress, Decompress, Verify, Create recovery data, and Repair buttons. Boundary analysis is
always automatic; there is no hardware-selection setting to misconfigure.
The GUI supports Japanese and English. It starts in Japanese when the Windows
UI language is Japanese and in English otherwise; the language can be changed
immediately from the controls in the upper-right corner. Labels, progress,
results, errors, and file dialogs are localized.
Compressed output is written beside the input with `.fst` appended. Decompressed
Fastener output gets its exact original file or folder name back whenever that
name is available. If the original still exists, the language-independent,
Windows-style collision-safe name is `sample (2).txt`, then `sample (3).txt`,
or a corresponding folder name. Fastener does not add restoration labels or a
fake extension.
Decompress and Verify accept both Fastener `.fst` and conventional ZIP archives.
ZIP contents use the archive stem as the folder name, or `archive (2)` when it
already exists.
Selecting a folder recursively stores the complete tree in one sibling `.fst`
file, like a ZIP archive. Each embedded file remains an independently compressed
and checksummed FST stream, and empty directories are preserved. Symlinks are
skipped.

Long operations show byte progress, percentage, elapsed time, worker count, and
live decimal MB/s, GB/s, Mbps, and Gbps rates.
Compression, decompression, and verification run on a background worker so the
Windows message loop remains responsive during multi-gigabyte operations.
The GUI configures its CPU worker pool to 75% of available logical processors,
leaving capacity for Windows and other applications. The CLI retains explicit
`--threads N` control. Completion dialogs report both the workers that actually
participated and the 75% pool limit. FST work is parallelized by independent
chunks; conventional ZIP work is parallelized by entries because one Deflate
entry is inherently a serial stream.

```console
fastener-gui.exe
```

## How it works

1. The analyzer samples 4 KiB blocks and scores pattern changes. When cuts are
   needed, CPU and wgpu split the samples within each block and run concurrently.
   GPU failure transparently switches that segment to full CPU analysis.
2. High-scoring positions near the target size become content-aware boundaries.
3. Rayon compresses independent chunks in parallel. Fastener applies the
   same content-aware hybrid boundaries to Fast (LZ4), Balanced (Zstd level 1),
   and Dense (Zstd level 12). Incompressible chunks are stored raw.
4. FST decompression is chunk-parallel and verifies every chunk with BLAKE3.
   Conventional ZIP extraction is parallel across independent ZIP entries.

GPU analysis affects only where chunks are cut. Compression and decompression
remain CPU-parallel, and archives never require a GPU. File commands use memory
mapping, 64 MiB analysis segments, and compression/decoding batches limited by
both chunk count and a 256 MiB data budget. The decoder rejects chunks larger
than 64 MiB; codec workspaces and mapped pages are additional memory costs.
Segments at most twice the target chunk size require
no cuts, so their scoring and GPU initialization are skipped without changing
boundaries. Larger segments retain the automatic hybrid analysis.
For files smaller than 16 MiB, any necessary scoring also runs entirely on the CPU.

## Build

Changes in the current release are recorded in [CHANGELOG.md](CHANGELOG.md).
The historical 1.0.0 benchmark below is retained as a historical result; the
1.0.2 comparison is linked in the debugging and performance section.

Requirements: a Rust toolchain compatible with the locked dependencies (validated
with Rust 1.96.1), a native C/C++ linker (Visual Studio Build
Tools on the MSVC toolchain, or MinGW/LLVM on the GNU toolchain), and a platform
supported by `wgpu`.

```console
cargo build --release
```

This builds both `fastener.exe` (CLI) and `fastener-gui.exe` (Windows GUI).
On Windows, `./build-release.ps1` also removes the build user's home-directory
prefix from Rust source paths embedded in the executables. Add `-Offline` when
all locked dependencies are already cached.

For a smaller CPU-only executable:

```console
cargo build --release --no-default-features
```

## OSS packages

Run `./package-oss.ps1` after `./build-release.ps1` to create the versioned
source-only ZIP and Windows ZIP in the parent directory. The Windows package
includes the same source tree and the CLI/GUI executables under `bin/`.
The source-only package contains no executables or build cache. Both contain
the MIT license, tests, locked dependencies, release notes, benchmark evidence,
and `SHA256SUMS.txt` for their contents. Dependency source code is not vendored.

## Commands

```console
# Compress (writes sample.txt.fst)
fastener compress sample.txt

# Change the target chunk size; analysis remains automatic
fastener compress large.bin -o large.fst --chunk-size 8388608

# Compression modes: 0 = fastest, 1 = balanced (default), 12 = dense
fastener compress large.bin --level 0
fastener compress large.bin --level 12

# Encrypt a file or folder; the password is prompted without echo
fastener compress documents --encrypt -o documents.fst
fastener verify documents.fst
fastener decompress documents.fst -o restored-documents

# Create a conventional single-entry Deflate/Zip64 archive (level 1 by default)
fastener zip-compress large.bin -o large.zip --level 1

# Decompress and verify while decoding
fastener decompress large.fst -o large-restored.bin

# Verify without creating an output file
fastener verify large.fst

# File-based round-trip benchmark (includes filesystem I/O)
fastener benchmark large.bin --iterations 5
```

Add `--threads N` before the subcommand to set the CPU worker count. Existing
output files are preserved unless `--force` is supplied.

## Try the included sample

```console
cargo run --release -- compress samples/fastener-demo.txt
cargo run --release -- verify samples/fastener-demo.txt.fst
cargo run --release -- decompress samples/fastener-demo.txt.fst -o restored.txt
```

Generate a deterministic 64 MiB mixed-pattern benchmark file:

```console
cargo run --release --example generate_sample -- benchmark.bin 64
fastener benchmark benchmark.bin --iterations 3
```

## 50 GiB実測 / 50 GiB benchmark

Fastener 1.0.0 Releaseで
`%USERPROFILE%\Downloads\deflate-stress-50GB.bin`（53,687,091,200 bytes、
50.00 GiB）を1回ずつ圧縮・解凍した実測です。2026-08-10、Intel Core
Ultra 7 265K（20 logical processors）、Windows 11 25H2 build 26200.8875、
NTFSの同一Cドライブで測定しました。FastenerはGUIと同じ75%にあたる
`--threads 15`、FSTは既定の8 MiBチャンク／Balanced level 1、Fastener ZIPは
Deflate level 1です。Windows標準ZIPにはWindows同梱の`tar.exe`（bsdtar
3.8.4 / libarchive 3.8.4）を使用しました。

All numbers below are one-run, end-to-end wall-clock measurements. Throughput is
calculated from the 53,687,091,200-byte original using decimal GB/s and Gbps.
Archive size uses GiB. Startup is included; separate SHA-256 time is excluded.

| 方式 / method | 書庫サイズ | 元サイズ比 | 圧縮時間 | 圧縮速度 | 解凍時間 | 解凍速度 |
|---|---:|---:|---:|---:|---:|---:|
| Windows標準 ZIP | 21.266 GiB | 42.532% | 1123.489 s | 0.048 GB/s / 0.382 Gbps | 374.693 s | 0.143 GB/s / 1.146 Gbps |
| Fastener ZIP (Deflate 1) | 27.211 GiB | 54.422% | 180.938 s | 0.297 GB/s / 2.374 Gbps | 289.234 s | 0.186 GB/s / 1.485 Gbps |
| Fastener FST (Balanced 1) | 3.020 GiB | 6.039% | 212.910 s | 0.252 GB/s / 2.017 Gbps | 147.148 s | 0.365 GB/s / 2.919 Gbps |

この入力ではFastener ZIP圧縮はWindows標準ZIPの6.21倍、展開は1.30倍でした。
FSTはWindows標準ZIPより圧縮が5.28倍、解凍が2.55倍で、書庫サイズは
Windows ZIPの14.2%です。圧縮＋解凍の合計はWindows ZIP 1498.182秒、
Fastener ZIP 470.172秒、FST 360.058秒でした。FSTは3方式で最小かつ
ラウンドトリップ最速ですが、Fastener ZIP単体の圧縮はFSTより速く、圧縮率との
トレードオフがあります。

3つの復元物はすべて元ファイルと同じSHA-256になりました。

```text
27982C97F357564AD84AF44982579A028E19452E5EC1524BEC6E48766637DF92
```

再現に使った主要コマンドは次のとおりです。PowerShell `Compress-Archive`は
基盤API由来の単一エントリ2 GiB制限があるため、この50 GiB測定には使わず、
同じくWindows同梱でZip64を扱える`tar.exe`をWindows標準値としました。

```console
tar.exe -a -cf windows-standard.zip -C %USERPROFILE%\Downloads deflate-stress-50GB.bin
tar.exe -xf windows-standard.zip -C windows-zip-restored

fastener.exe --threads 15 zip-compress deflate-stress-50GB.bin -o fastener.zip --level 1
fastener.exe --threads 15 decompress fastener.zip -o fastener-zip-restored

fastener.exe --threads 15 compress deflate-stress-50GB.bin -o fastener.fst --level 1 --chunk-size 8388608
fastener.exe --threads 15 decompress fastener.fst -o fst-restored.bin
```

測定は元ファイルSHA-256、FST圧縮、Fastener ZIP、Windows ZIPの順で行い、
最終版のFST解凍は順序付き書き込みへの最適化後に最後に再測定しました。OSキャッシュを
消去せず、ウイルス対策やバックグラウンド負荷も固定していない単発測定なので、
厳密なCPUコーデック・ベンチマークではありません。特に全方式が同じドライブを
読み書きするため、結果にはストレージ帯域とキャッシュが強く含まれます。

The 10 GB/s ZIP and 100 GB/s FST figures are design ceilings for sufficiently
parallel, memory-resident workloads, not claims about this disk-backed run.
This machine measured at most 0.186 GB/s for ZIP extraction and 0.365 GB/s for
FST extraction end to end. A 100 GB/s result requires a separate memory-only
codec benchmark and hardware with enough aggregate memory bandwidth; it must
not be presented as achieved disk throughput.

The exact numeric record is also available as
[`benchmarks/deflate-stress-50GB-2026-08-10.json`](benchmarks/deflate-stress-50GB-2026-08-10.json).

## Test

```console
cargo test --all-features
cargo test --no-default-features
```

The tests cover multi-chunk and empty round trips, corruption rejection,
malformed input, boundary invariants, and the complete CLI workflow. Regression
tests also cover whole-file hash corruption (including empty archives), preserving
existing output after failed decoding or interrupted FST/ZIP compression, bounded
batches, invalid compression levels, and GPU scoring with unaligned input tails.

## Debugging and performance update (2026-09-23)

File verification and decompression now check the whole-file BLAKE3 digest as
well as individual chunks. FST compression, file decompression, and ZIP creation
write to a temporary sibling file and replace the requested output only after
success. Directory and ZIP extraction now also use temporary sibling directories
and publish only after all entries validate. Directory FST creation uses a
temporary sibling file as well.

GPU uploads no longer assemble every byte into a separate u32 staging array.
Raw chunks are read directly from the archive mapping during file decoding and
verification. Large whole-file hash updates use the existing Rayon worker pool.
The archive format and content boundary algorithm are unchanged.

The reproducible comparison and measured results are in
[`benchmarks/debug-speed-2026-09-23/REPORT.md`](benchmarks/debug-speed-2026-09-23/REPORT.md).

## Current prototype limits

- File FST and directory FST use distinct magic signatures. Directory FST stores
  a safe relative-path manifest plus independently verified embedded FST streams.
- Directory archives are limited to 100,000 entries and 4,096-byte UTF-8 paths.
  ZIP verification/extraction accepts at most 100,000 entries and 64 GiB of
  declared expanded data by default. For larger known ZIP archives, use
  `--zip-max-entries N` and `--zip-max-output-bytes BYTES` with `verify`,
  `decompress`, or ZIP `repair` to raise those limits. Windows device names, alternate-stream separators,
  and ambiguous trailing dots/spaces are rejected across archive types.
- The automatic GPU scorer is intentionally approximate and needs real-world tuning.
- Level 0 chunks use LZ4. Levels 1 through 22 use Zstd at the selected level;
  automatic segmentation, parallel orchestration, and the verified container
  are Fastener's format-level additions.
- Encryption is optional and uses the separate FSTENC01 format. Older Fastener
  versions cannot read it. There are no digital signatures or random-access
  streaming index. Separate recovery sidecars are available. Conventional ZIP
  output is not encrypted.
- ZIP 10 GB/s and FST 100 GB/s are ceiling targets for sufficiently parallel,
  memory-resident workloads. They are not end-to-end guarantees: Deflate stream
  dependencies, file distribution, CPU, memory bandwidth, codec ratio, and
  storage speed determine observed throughput. The GUI always reports measured
  throughput instead of presenting a target as a result.

See [FORMAT.md](FORMAT.md) for the exact portable container layout.

