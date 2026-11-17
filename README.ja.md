# Fastener

[English](README.md)

Fastenerは、ファイルやフォルダーをまとめるMITライセンスの書庫ツールです。LZ4またはZstandardで並列圧縮したFST書庫を作成・検証できます。パスワード暗号化や、FST・ZIP書庫に対応する別ファイルの復旧データも利用できます。

Windowsでは `fastener-gui.exe` を起動します。CLIでも同じ書庫を扱えます。

~~~console
fastener compress samples/fastener-demo.txt
fastener verify samples/fastener-demo.txt.fst
fastener decompress samples/fastener-demo.txt.fst -o restored.txt
~~~

- [使い方とビルド](docs/ja/usage.md)
- [圧縮・展開の仕組み](docs/ja/architecture.md)
- [FSTのバイナリ形式](docs/ja/format.md)
- [暗号化書庫](docs/ja/encryption.md)
- [復旧データの形式](docs/ja/recovery-format.md)
- [ベンチマーク](docs/ja/benchmarks.md)
- [変更履歴](docs/ja/changelog.md)
- [MITライセンス](LICENSE)
- [依存ライブラリのライセンス一覧](LICENSES/README.md)
