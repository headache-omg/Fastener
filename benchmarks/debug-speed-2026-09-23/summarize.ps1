$ErrorActionPreference = 'Stop'
$records = Get-Content (Join-Path $PSScriptRoot 'measurements.json') -Raw | ConvertFrom-Json
$manifest = Get-Content (Join-Path $PSScriptRoot 'manifest.json') -Raw | ConvertFrom-Json
function Median($values) {
    $ordered = @($values | Sort-Object)
    if ($ordered.Count % 2) { return $ordered[[int][Math]::Floor($ordered.Count / 2)] }
    return ($ordered[$ordered.Count / 2 - 1] + $ordered[$ordered.Count / 2]) / 2
}
$summary = foreach ($group in ($records | Group-Object Case)) {
    $before = @($group.Group | Where-Object { !$_.Warmup -and $_.Variant -eq 'before' })
    $after = @($group.Group | Where-Object { !$_.Warmup -and $_.Variant -eq 'after' })
    if ($before.Count -ne $manifest.Iterations -or $after.Count -ne $manifest.Iterations) { throw 'Incomplete measurements' }
    if (@($group.Group | Where-Object { $_.InputSha256 -ne $_.RestoredSha256 }).Count) { throw 'Restoration mismatch' }
    if (@($group.Group.ArchiveSha256 | Sort-Object -Unique).Count -ne 1) { throw 'Archive bytes changed across variants' }
    $beforeC = Median $before.CompressMs
    $afterC = Median $after.CompressMs
    [ordered]@{
        Case = $group.Name; InputMiB = $before[0].InputBytes / 1MB
        BeforeCompressMs = $beforeC; AfterCompressMs = $afterC
        CompressReductionPercent = (1 - $afterC / $beforeC) * 100
        BeforeDecompressMs = Median $before.DecompressMs; AfterDecompressMs = Median $after.DecompressMs
        BeforeVerifyMs = Median $before.VerifyMs; AfterVerifyMs = Median $after.VerifyMs
        ArchiveBytes = $before[0].ArchiveBytes
    }
}
$summary | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'summary.json') -Encoding utf8
$lines = [Collections.Generic.List[string]]::new()
$lines.Add(@'
# Fastener 1.0.2 — デバッグと高速化の検証

2026-09-23。今回の作業開始時にあった `oss` のソース（1.0.0）を独立してビルドした修正前EXEと、修正後1.0.2のEXEを比較。過去の配布ZIPとの比較ではありません。

## 修正内容

- FSTファイルの検証・展開で、チャンクだけでなくファイル全体のBLAKE3も照合。空ファイルも検証対象。
- FST圧縮・ファイル展開・ZIP作成は同じフォルダの一時ファイルに書き、成功後だけ出力先を置換。エラーや処理中断による既存出力の破壊を防止。
- 圧縮・展開・検証のバッチを256 MiBのデータ量とチャンク数で制限。単一チャンクがこれを超える場合は単独処理。コーデック内部メモリとマッピングは別途必要。
- GPUへの入力を直接転送し、1バイトずつu32配列に変換する処理を削除。GPU上限超過時はCPUへ切り替え。
- 分割が不要な区間（長さが目標チャンクサイズの2倍以下）では採点とGPU起動を省略。境界位置は変化しない。
- ファイル展開・検証のrawチャンクはマッピングから直接参照し、不要なコピーを削除。大きい全体ハッシュ計算は並列化。
- 不正なチャンク情報と圧縮レベルを早期拒否。CLIベンチマークの一時領域を固有ディレクトリに変更し、失敗時も片付ける。

## 測定

Windows、20論理プロセッサ、15ワーカー、Releaseビルド。各ケース1回ウォームアップ後、修正前・修正後の順番を交互にして5回ずつ測定した中央値。プロセス起動とファイル入出力を含み、測定後のSHA-256照合は時間に含めない。OSキャッシュは消去していない。256 MiB混合データは規則的データ・ゼロ・疑似乱数、128 MiBデータは疑似乱数。生成手順は `run.ps1` に収録。

単位はミリ秒。短いほど高速。

| ケース | 入力 | 圧縮 前→後 | 圧縮時間短縮 | 展開 前→後 | 検証 前→後 |
|---|---:|---:|---:|---:|---:|
'@)
foreach ($row in $summary) {
    $lines.Add(('| {0} | {1} MiB | {2:F1} → {3:F1} | {4:F1}% | {5:F1} → {6:F1} | {7:F1} → {8:F1} |' -f $row.Case, $row.InputMiB, $row.BeforeCompressMs, $row.AfterCompressMs, $row.CompressReductionPercent, $row.BeforeDecompressMs, $row.AfterDecompressMs, $row.BeforeVerifyMs, $row.AfterVerifyMs))
}
$lines.Add(@'

全48回（ウォームアップを含む）の復元SHA-256が元データと一致。各ケースで書庫サイズと書庫SHA-256も修正前後で一致した。

展開・検証は従来省略されていた全体ハッシュ検証を追加しており、一部で時間が増加した。すべての操作が速くなったわけではない。小さい入力の圧縮改善はGPUの起動待ちを省いた効果が大きい。50 GiBでの再計測、キャッシュを排除した測定、厳密なメモリピーク測定は今回実施していない。

## 不具合の再現

`regressions.json` に保存した比較結果:

| 破損 | 修正前 | 修正後 |
|---|---|---|
| 全体チェックサムだけを変更 | verify・decompressが成功扱いになり、既存出力を置換 | 両方とも失敗し、既存出力を保持 |
| チャンクチェックサムを変更 | 展開は失敗するが、既存出力も失われる | 展開は失敗し、既存出力を保持 |

## 検査結果と範囲

- `cargo test --release --all-features --locked --offline`: 31件成功。
- `cargo test --release --no-default-features --locked --offline`: 29件成功。
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`: 成功。
- `cargo fmt --all -- --check`: 成功。
- NVIDIA GeForce RTX 5070 Tiで、GPUとCPUの境界スコア一致を実際に確認。非整列の末尾についてもテスト。
- Rust/Cargo 1.96.1。配布用EXEはビルドパスを置換して生成。
- 既存のディレクトリ書庫・ZIP展開の動作テストは成功。ただしディレクトリ全体の展開を一括置換する仕組みは今回の変更に含まれない。
- GUIは共通エンジンと既存6件のGUIロジックテストで検証。EXEの起動・入力待機・応答も確認（gui-startup.json）。画面上の操作を通した一連の圧縮・展開は未検証。

## 再現方法

ローカル比較用の `bin/before.exe` と `bin/after.exe` に対して:

```powershell
./run.ps1 -Iterations 5 -Threads 15
./reproduce.ps1
./summarize.ps1
```

比較用EXEを再ビルドする場合、`before/Cargo.toml` は修正前ソース。そこで `cargo build --release --locked` を実行して `bin/before.exe` に配置。現在のソースを `build-release.ps1` でビルドして `bin/after.exe` に配置する。配布ZIPは旧EXEと生成データを含まない。

詳細は `measurements.json`、`summary.json`、`manifest.json`、`regressions.json` を参照。manifestに比較したEXEのSHA-256を保存。
'@)
$lines | Set-Content (Join-Path $PSScriptRoot 'REPORT.md') -Encoding utf8
$summary | ForEach-Object { [pscustomobject]$_ } | Format-Table Case, BeforeCompressMs, AfterCompressMs, CompressReductionPercent, BeforeDecompressMs, AfterDecompressMs
