# FSTPAR01 binary format (version 1)

All integers are little-endian. This is an independent recovery sidecar, not a change to FST archive formats. GF(2^8) encoding uses `reed-solomon-erasure` 6.0.0 with 20 data and 2 parity shards in that order.

Layout: `header`, repeated (`index`, `parity 0`, `parity 1`, `index copy`), `header copy`.

The header is exactly 128 bytes:

| Offset | Bytes | Meaning |
|---:|---:|---|
| 0 | 8 | ASCII FSTPAR01 |
| 8 | 4 | version = 1 |
| 12 | 4 | shard size, power of two in [4096, 1048576] |
| 16 | 8 | original archive byte length |
| 24 | 8 | group count = ceil(original length / (20 × shard size)), max 1,000,000 |
| 32 | 32 | BLAKE3 of original archive bytes |
| 64 | 1 | kind: 0=file FST, 1=directory FST, 2=ZIP, 3=encrypted FST |
| 65 | 31 | reserved, all zero |
| 96 | 32 | BLAKE3 of preceding 96 header bytes |

Each index is exactly 744 bytes: group number (u64), 22 BLAKE3 hashes (32 bytes each, data then parity order), and BLAKE3 of the preceding 712 index bytes. Shard hashes cover the entire shard including known zero padding. No original names, paths, passwords, or keys are stored.

The first index of group g is at `128 + g × (1488 + 2 × shard_size)`. Its second copy follows the two parity shards. The footer is at `128 + group_count × (1488 + 2 × shard_size)`. This fixed geometry permits seeking without trusting a corrupted index or scanning for magic bytes.

When the first header is invalid, a valid header exactly 128 bytes before physical EOF may recover the geometry, provided its predicted sidecar length equals the physical length. With a valid first header, a missing/truncated footer is allowed. Index copies use checksums and group numbers; valid but conflicting copies are rejected. Missing/truncated parity is treated as erasure. Unknown header fields or invalid limits are rejected before shard allocation.

Data offsets are fixed: `(g × 20 + shard_number) × shard_size`. Unavailable original bytes inside the saved original length are erasures. Bytes beyond the saved length are known-zero padding. Unexpected physical read errors abort; only short reads are treated as erasures. Insertions/deletions in the middle are not resynchronized.

Repair writes only to a temporary sibling, validates every reconstructed data shard and the original whole-file hash, then runs the original archive verifier (including password-based AEAD verification for encrypted archives) before publication. Checksums are not sender authentication.

The reconstructed file's leading archive magic must agree with the declared kind before selecting a verifier. A valid ZIP trailer cannot downgrade an encrypted-magic archive to ZIP-only validation.

