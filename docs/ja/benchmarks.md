# ベンチマーク

[English](../en/benchmarks.md) · [README](../../README.ja.md)

1.2.3の比較ではWindowsのリリースビルド、同一ドライブ、目標チャンク8 MiB、Zstandardレベル1を使用しました。2 GiBの測定では8ワーカーでウォームアップし、各版を交互に4回ずつ実行しました。OSキャッシュは保持しています。展開時間の中央値は **1.233秒から0.625秒**、圧縮時間は **1.496秒と1.490秒** でした。復元したファイルのSHA-256はすべて入力と一致しました。

別の50 GiB入力では、15ワーカーで1.2.3の配布ビルドを先に実行して76.937秒、その後の1.2.2は164.846秒でした。各版1回の測定で、ドライブやキャッシュの状態の影響を受けます。過去の1.0.0とZIPの比較とは入力が異なります。

- [2 GiBの各試行の計測値](../../benchmarks/v1.2.3/measurements-reproduced.json)
- [実行ファイルと計測条件](../../benchmarks/v1.2.3/manifest-reproduced.json)
- [1.0.0とZIPの過去の計測値](../../benchmarks/deflate-stress-50GB-2026-08-10.json)

手元の入力では `fastener benchmark FILE --iterations 3` を実行します。ファイル入出力込みで圧縮と展開を計測し、計測区間の外で復元結果を検証します。

## 過去の測定レポート

- [debug-speed-2026-09-23](benchmarks/debug-speed-2026-09-23.md)
- [v1.1.0](benchmarks/v1.1.0.md)
- [v1.1.1](benchmarks/v1.1.1.md)
- [v1.2.0](benchmarks/v1.2.0.md)
- [v1.2.1](benchmarks/v1.2.1.md)
- [v1.2.2](benchmarks/v1.2.2.md)
- [v1.2.3](benchmarks/v1.2.3.md)
