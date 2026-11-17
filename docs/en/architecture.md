# Compression and extraction

[日本語](../ja/architecture.md) · [README](../../README.md)

- **Boundary selection.** Fastener scores changes among samples from 4 KiB blocks and chooses boundaries near the target size. File operations analyze 64 MiB segments. A segment at most twice the target needs no scoring. Otherwise, inputs of at least 16 MiB can use wgpu alongside the CPU. GPU failure falls back to CPU scoring.
- **Compression.** Rayon compresses independent chunks using LZ4 or Zstandard. The writer stores the original bytes when compression does not shrink a chunk. GPU work ends at boundary selection. An FST reader can restore the archive using only the CPU.
- **Large files.** The ordinary file path uses memory mapping and batches capped by chunk count and a 256 MiB data budget. The budget covers batch data. Mapped pages, codec workspaces, and metadata can add memory use. During extraction, workers decode into disjoint parts of a temporary mapped output. The decoder hashes batches in file order and publishes after chunk and whole-file BLAKE3 checks pass.
- **Folders and ZIP.** Ordinary folder FST stores independent FST streams and preserves empty directories. Symbolic links are skipped by the folder writer. Folder FST and ZIP extraction stage output in temporary sibling directories until validation succeeds.
- **Encrypted archives.** Fastener authenticates each record and verifies each restored file. Verification writes no plaintext files. Extraction stages plaintext before publication. [Encrypted format](encryption.md)
