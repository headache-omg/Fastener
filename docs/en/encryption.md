# Encrypted FST archives

[日本語](../ja/encryption.md) · [Usage](usage.md)

Use `fastener compress INPUT --encrypt` or enable encryption in the Windows GUI. The CLI prompts twice without echo during compression and once during extraction, verification, or repair. For automation use `--password-file PATH` with a protected, single-line UTF-8 file. Fastener strips a leading BOM and one final LF or CRLF. Passwords must be 1–1024 UTF-8 bytes.

## Binary format

Integers are unsigned little-endian. The 48-byte header contains:

| Offset | Bytes | Field |
|---:|---:|---|
| 0 | 8 | ASCII `FSTENC01` |
| 8 | 2 | Version 1 |
| 10 | 1 | Kind, 0 file or 1 directory |
| 11 | 1 | KDF profile 1 |
| 12 | 4 | Target chunk size, 8 KiB to 8 MiB |
| 16 | 16 | Random Argon2id salt |
| 32 | 16 | Random nonce prefix |

Profile 1 uses Argon2id v19 with 64 MiB, three iterations, one lane, and 32 output bytes. XChaCha20-Poly1305 uses a 24-byte nonce composed of the prefix and the implicit zero-based record number as a u64. Each frame is a u32 ciphertext length followed by ciphertext including a 16-byte authentication tag. AAD is the header, record number, and ciphertext length, in that order.

| Plaintext tag | Fields after the tag |
|---|---|
| `M` | u32 entry count, then u8 kind, u64 original size, u32 path length, and relative UTF-8 path for each entry |
| `D` | u32 decoded length, u8 codec (0 raw, 1 Zstandard, 2 LZ4), then payload |
| `H` | 32-byte BLAKE3 of the whole restored file |
| `E` | End of container |

The manifest comes first. Each regular file has zero or more data records and one hash record in manifest order. Directories have neither data nor hash records. The end record is mandatory and trailing bytes are rejected. The writer and reader cap the manifest at 8 MiB and 100,000 entries. The path limit is 4,096 bytes, decoded data records are capped at 16 MiB, and frames at 16 MiB plus 64 bytes. The default expanded-size limit is 64 GiB.

## Visible information and output

The outer filename, archive size, file/directory kind, and ciphertext frame lengths remain visible. Internal paths, declared file sizes and hashes are encrypted. Extraction stages plaintext next to the output and publishes it after validation. Abrupt termination can leave staging files. Verification writes no plaintext files. The archive protocol has had internal review and has yet to receive an independent audit. A matching recovery sidecar and the correct password are required to repair a damaged encrypted archive. [Recovery format](recovery-format.md)
