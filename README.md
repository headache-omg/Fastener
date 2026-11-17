# Fastener

[日本語](README.ja.md)

Fastener is an MIT-licensed archiver for files and folders. It creates and verifies FST archives with parallel LZ4 or Zstandard compression, supports optional password encryption, and can generate separate recovery data for FST and ZIP archives.

Run `fastener-gui.exe` for the Windows interface or use the CLI:

~~~console
fastener compress samples/fastener-demo.txt
fastener verify samples/fastener-demo.txt.fst
fastener decompress samples/fastener-demo.txt.fst -o restored.txt
~~~

- [Usage and builds](docs/en/usage.md)
- [Compression and extraction](docs/en/architecture.md)
- [FST binary format](docs/en/format.md)
- [Encrypted archives](docs/en/encryption.md)
- [Recovery sidecar format](docs/en/recovery-format.md)
- [Benchmarks](docs/en/benchmarks.md)
- [Changelog](docs/en/changelog.md)
- [MIT license](LICENSE)
- [Dependency licenses](LICENSES/README.md)
