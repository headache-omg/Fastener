# Fastener container format v1

All integers are unsigned and little-endian. Hashes are 32-byte BLAKE3 digests.

## File header (60 bytes)

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | ASCII magic `FASTENR1` |
| 8 | 2 | format version (`1`) |
| 10 | 2 | flags (reserved, zero) |
| 12 | 8 | original byte length |
| 20 | 4 | chunk count |
| 24 | 4 | target chunk size used by the encoder |
| 28 | 32 | BLAKE3 of the complete original file |

## Chunk records

Records immediately follow the header and are stored in original-file order.

| Size | Field |
|---:|---|
| 8 | original offset |
| 4 | original length |
| 4 | stored payload length |
| 1 | codec: `0` raw, `1` Zstandard, `2` LZ4 block |
| 3 | reserved, zero |
| 32 | BLAKE3 of the uncompressed chunk |
| variable | payload |

The decoder rejects gaps, overlaps, truncation, trailing bytes, unsupported codecs,
chunk hash failures, and whole-file hash failures. Analyzer metadata is deliberately
not stored: hardware only influences boundaries and is never required for decoding.
Current decoders reject decoded chunks larger than 64 MiB to bound allocations
from untrusted archive records. The file encoder produces at most 64 MiB chunks.

Codec 2 was added by Fastener 0.3.0 without changing the container version. Older
codec-0/1 archives remain readable. New 0.3.0 archives prioritize codec 2 for
parallel decode speed.

## Directory container v1

A folder is stored in one file with the distinct ASCII magic `FASTDIR1`. All
paths are UTF-8, relative, and use `/` separators. Absolute paths, parent
components, duplicate paths, symbolic links, Windows device names, alternate
stream separators, and trailing dots/spaces are rejected. Current readers limit
archives to 100,000 entries and each encoded path to 4,096 bytes. The declared
entry count must also fit the physical archive length.

### Directory header (28 bytes)

| Size | Field |
|---:|---|
| 8 | ASCII magic `FASTDIR1` |
| 2 | directory format version (`1`) |
| 2 | flags (reserved, zero) |
| 4 | total entry count |
| 4 | regular-file count |
| 8 | combined original file size |

### Directory entry

| Size | Field |
|---:|---|
| 1 | kind: `0` regular file, `1` directory |
| 3 | reserved, zero |
| 4 | UTF-8 path length |
| 8 | original file size, or zero for a directory |
| 8 | embedded FST size, or zero for a directory |
| variable | relative UTF-8 path |
| variable | complete `FASTENR1` stream for a regular file |

Embedding independent file streams preserves per-file checksums and allows a
damaged entry to be identified without changing the original file format.

## Encrypted container (Fastener 1.1.0)

Optional encrypted archives use the distinct magic `FSTENC01`, version 1,
with Argon2id password derivation and XChaCha20-Poly1305 authenticated records.
File names and whole-file checksums are encrypted as well as compressed data.
The byte layout, authentication rules, limits, and password handling are specified
in [暗号化仕様.md](docs/暗号化仕様.md). FASTENR1/FASTDIR1 remain unchanged.

Optional recovery sidecars use a separate format: [FSTPAR01](docs/RECOVERY_FORMAT.md).

