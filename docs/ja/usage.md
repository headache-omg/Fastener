# Fastenerの使い方

[English](../en/usage.md) · [README](../../README.ja.md)

Windows GUIは日本語と英語に対応します。ファイルまたはフォルダーを選び、圧縮モードを指定して「圧縮」を押します。書庫を選ぶと解凍、検証、復旧データの作成、修復ができます。処理中は進捗、経過時間、速度、ワーカー数を表示します。GUIのCPUワーカー枠は論理プロセッサー数の75%を切り捨てた値で、最小は1です。

## CLIコマンド

| コマンド | 動作 |
|---|---|
| `compress INPUT` | ファイルまたはフォルダーをFSTに圧縮。既定では入力名に `.fst` を追加 |
| `zip-compress INPUT` | 単一ファイルをZIPに圧縮。既定では拡張子を `.zip` に変更 |
| `decompress ARCHIVE` | FSTまたはZIPをファイル・フォルダーに展開 |
| `verify ARCHIVE` | 復元物を確定せず、書庫を読み出して検証 |
| `recovery-create ARCHIVE` | `ARCHIVE.par` を作成。`--estimate` で書き込みなしの容量見積もり |
| `repair ARCHIVE` | 対応する復旧データを使い、`data.repaired.fst` などの別ファイルに修復 |
| `benchmark FILE` | ファイル入出力を含めて圧縮・展開時間を計測し、往復結果を検証 |

~~~console
fastener --threads 8 compress documents --level 12
fastener compress documents --encrypt -o private.fst
fastener verify private.fst
fastener decompress private.fst -o restored-documents
fastener zip-compress report.txt -o report.zip
fastener recovery-create private.fst --estimate
fastener recovery-create private.fst
fastener repair private.fst
~~~

FSTのレベル0はLZ4、1〜22はZstandardです。Balanced（既定）は1、Denseは12を使います。目標チャンクサイズの既定値は8 MiBで、暗号化時は8 KiB〜8 MiBを指定できます。ZIP作成のDeflateレベルは0〜9で、既定は1です。ZIPの各エントリーは直列で処理し、独立したエントリーを並列化します。

`-o` を省略すると書庫名から展開先を決め、同名の項目があれば ` (2)` などを付けます。ZIPは書庫の拡張子を外した名前のフォルダーに展開します。フォルダー展開の出力先を指定する場合は、新しいパスを使います。ファイルの上書きに対応する操作では `--force` を指定します。

現行ソースの既定の展開後容量上限は書庫ごとに64 GiBです。信頼できる大容量書庫では、`verify`、`decompress`、`repair` に形式に応じて `--fst-max-output-bytes`、`--encrypted-max-output-bytes`、`--zip-max-output-bytes` を指定できます。ZIPには既定で100,000件のエントリー上限があり、`--zip-max-entries` で変更できます。

破損前に作った `.par` を保管してください。各グループに十分な正常シャードが残っていれば、最大2シャードの欠損や破損を補えます。同じグループで3つ以上が壊れると修復できない場合があります。修復後に書庫を検証し、暗号化書庫ではパスワードも必要です。復旧データを別の場所に置いた場合は `--recovery PATH` を指定します。[復旧データの形式](recovery-format.md)

## ビルドと配布

Rustとネイティブツールチェーン（Windows MSVCの場合はVisual Studio Build Toolsなど）を用意します。CLIはWindowsとLinuxでビルドでき、GUIはWindowsで実行できます。

~~~console
cargo build --release --locked
cargo build --release --locked --no-default-features
~~~

2番目のコマンドはGPUによる境界分析を省いたビルドです。Windowsでは `./build-release.ps1` に続けて `./package-oss.ps1` を実行すると、ソース版とWindows版のZIPを作成できます。Windows版の `bin/` にCLIとGUIが入ります。
