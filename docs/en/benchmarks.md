# Benchmarks

[日本語](../ja/benchmarks.md) · [README](../../README.md)

The 1.2.3 comparison used Windows release builds, the same drive, an 8 MiB target chunk size, and Zstandard level 1. The 2 GiB test used eight workers, four alternating measured runs per build after warm-up, and retained the OS cache. Median extraction went from **1.233 s to 0.625 s**. Median compression was **1.496 s vs. 1.490 s**. All restored copies matched the input SHA-256.

A separate 50 GiB trial using 15 workers measured 76.937 s for the 1.2.3 distribution build followed by 164.846 s for 1.2.2. These were single runs. Disk and cache conditions affect the comparison. Its fixture differs from the historical 1.0.0 ZIP comparison.

- [2 GiB run data](../../benchmarks/v1.2.3/measurements-reproduced.json)
- [Build and measurement manifest](../../benchmarks/v1.2.3/manifest-reproduced.json)
- [Historical 1.0.0 ZIP comparison data](../../benchmarks/deflate-stress-50GB-2026-08-10.json)

To measure your own input, run `fastener benchmark FILE --iterations 3`. The command times compression and extraction including file I/O, then checks the restored file outside the timing.
