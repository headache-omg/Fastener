# Changelog

## Unreleased

- Extract ZIP and Fastener directory archives into temporary sibling directories,
  then publish only after every entry validates. Failed extraction no longer leaves
  partial output directories behind.

## 1.2.3 — 2026-09-26

- Decode ordinary single-file FST chunks directly into nonoverlapping regions
  of a temporary memory-mapped output, avoiding the decoded batch buffers and
  serial copy into a buffered writer.
- Hash each bounded output batch immediately after parallel chunk validation,
  so a large output is not reread from storage for its whole-file checksum.
- Keep temporary-output publication and all chunk/whole-file integrity checks.
- Correct the encryption guide's obsolete statement about future recovery data.
- Add a measured 50 GiB old/new extraction comparison and restored SHA-256 checks.

## 1.2.2 — 2026-09-26

- Reuse one Zstd compression workspace per Rayon worker during FST file
  compression, instead of rebuilding it for every chunk.
- Borrow incompressible chunks directly from the mapped input while writing FST
  archives, avoiding an additional full-size allocation and copy per raw chunk.
- Reuse the output scratch buffer for raw Zstd chunks and overlap whole-file
  hashing with boundary analysis for files of at least 128 MiB.
- Preserve the FST byte format, archive checksums, bounded batches, and atomic output.
- Add a large-file old/new comparison with restored-file SHA-256 validation.

## 1.2.1 — 2026-09-26

- Parallelized Reed–Solomon encoding/reconstruction over borrowed 64 KiB stripes
  for shards at least 256 KiB, preserving the FSTPAR01 byte format.
- Reuse the bounded shard buffers across groups and parallelize large shard hashes
  and whole-file hash updates. Keep all verification and authentication checks.
- Added independent serial-reference parity/reconstruction tests, including tails
  and a single-worker pool, plus an old/new recovery benchmark.

## 1.2.0 — 2026-09-25

- Implemented optional FSTPAR01 sidecars with Reed–Solomon 20+2 recovery and adaptive
  4 KiB–1 MiB shards, per-shard BLAKE3, duplicated headers and group indexes.
- Added GUI recovery creation/repair and CLI `recovery-create`, `--estimate`, `repair`.
- Repair restores a separate temporary archive, verifies its whole-file hash and
  original archive format, and authenticates encrypted archives before publication.
- Added corruption, truncation, metadata redundancy, parity loss, capacity overflow,
  wrong-password, interruption, CLI and real GUI-worker coverage.
- Recovery remains opt-in. Middle insertion/deletion resynchronization is not supported.

## 1.1.1 — 2026-09-25

- GPU scoring uploads only the exact sampled bytes, reducing input transfer
  from 64 MiB to 4 MiB plus a length word per full analysis segment.
- Parallelized CPU scoring for segments with at least 256 analysis blocks.
- Kept boundary scores, archive formats, encryption parameters, and integrity checks unchanged.
- Added a serial-reference regression test and expanded GPU tail-length coverage.


## 1.1.0 — 2026-09-24

- Added Japanese documentation of the algorithms, implementation choices, and limits.
- Inputs below 16 MiB use full CPU scoring even when a small chunk size requires cuts.
- Added optional, directly streamed file/folder encryption using Argon2id and
  XChaCha20-Poly1305 with parallel authenticated records and encrypted manifests.
- Added GUI encryption/password controls and CLI `--encrypt` / `--password-file`.
  Interactive CLI passwords are hidden; GUI fields are masked and cleared on start.
- Authenticated termination and whole-file hashes reject corruption, truncation,
  record reordering, and cross-archive substitution. Failed extraction preserves output.
- Encrypted directory extraction is staged and published after complete validation.
- Added an in-memory fixed-vs-content-boundary comparison example.
- Recovery records remain unimplemented; a Japanese sidecar-parity proposal is included.
- The new encrypted protocol is not independently security-audited. Older clients
  cannot read encrypted archives; unencrypted formats remain compatible.

## 1.0.2 — 2026-09-23

### Fixed

- Validate the whole-file BLAKE3 digest during file verification and extraction,
  including empty files.
- Preserve existing output after failed or interrupted FST compression,
  file extraction, and ZIP creation by publishing a temporary sibling on success.
- Reject invalid compression levels, malformed chunk records, and impossible
  record counts before doing expensive work.
- Fall back to CPU analysis when the input exceeds GPU buffer or dispatch limits.
- Give the CLI benchmark a unique, automatically cleaned temporary directory;
  avoid truncating its iteration count when averaging durations.

### Performance

- Upload GPU input bytes directly, removing per-byte u32 staging conversion.
- Skip scoring and GPU initialization when no content boundary can be selected.
- Borrow raw file chunks from the archive mapping instead of copying them.
- Parallelize large whole-file hash updates.
- Bound work batches by both chunk count and a 256 MiB data budget. A single
  oversized chunk and codec-internal allocations can exceed that budget.

Compression took 12–25% less time for the measured 128–256 MiB inputs and 83%
less for the 1 MiB input. Some extraction and verification cases take longer
because they now perform the previously missing whole-file validation.
These are cached-filesystem measurements on one machine, not universal guarantees.
See [the full comparison](benchmarks/debug-speed-2026-09-23/REPORT.md).

### Validation and compatibility

- 31 GPU-enabled and 29 CPU-only tests passed; Clippy and formatting checks passed.
- GPU/CPU scoring agreement checked on NVIDIA GeForce RTX 5070 Ti.
- All 48 benchmark restorations matched their input SHA-256; archive bytes
  were identical between the compared implementations for each fixture.
- The FASTENR1 and FASTDIR1 formats remain unchanged.
- Directory and ZIP extraction are not transactional as a whole.

This release was developed from the local 1.0.0 source snapshot. The retained
historical 1.0.0 benchmark is separate from this release's comparison.
