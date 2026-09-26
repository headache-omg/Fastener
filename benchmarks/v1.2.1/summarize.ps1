$ErrorActionPreference = 'Stop'
function Median($values) { $a=@($values | Sort-Object); $i=[int][Math]::Floor($a.Count/2); if ($a.Count%2) { return $a[$i] }; return ($a[$i-1]+$a[$i])/2 }
$rows=Get-Content (Join-Path $PSScriptRoot 'measurements.json') -Raw | ConvertFrom-Json
$cli=foreach ($group in ($rows | Group-Object Case)) {
    if (@($group.Group | Where-Object { $_.ArchiveSha256 -ne $_.RestoredSha256 }).Count) { throw 'Restoration mismatch' }
    if (@($group.Group.RecoverySha256 | Sort-Object -Unique).Count -ne 1) { throw 'Recovery file mismatch' }
    foreach ($variant in @('before','after')) {
        $r=@($group.Group | Where-Object { $_.Variant -eq $variant -and !$_.Warmup })
        if ($r.Count -ne 5) { throw 'Incomplete CLI measurements' }
        [ordered]@{Case=$group.Name;Variant=$variant;CreateMs=Median $r.CreateMs;RepairMs=Median $r.RepairMs;
            CreatePeakMiB=($r.CreatePeakWorkingSetBytesObserved | Measure-Object -Maximum).Maximum/1MB;
            RepairPeakMiB=($r.RepairPeakWorkingSetBytesObserved | Measure-Object -Maximum).Maximum/1MB}
    }
}
$cli | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'summary.json') -Encoding utf8
$rows=Get-Content (Join-Path $PSScriptRoot 'engine-measurements.json') -Raw | ConvertFrom-Json
$engine=foreach ($case in @('small','large')) {
    $group=@($rows | Where-Object Case -eq $case)
    if (@($group.parity_blake3 | Sort-Object -Unique).Count -ne 1) { throw 'Engine recovery mismatch' }
    if (@($group.archive_blake3 | Sort-Object -Unique).Count -ne 1) { throw 'Engine input mismatch' }
    $before=@($group | Where-Object { $_.Variant -eq 'before' -and $_.iteration -gt 0 })
    $after=@($group | Where-Object { $_.Variant -eq 'after' -and $_.iteration -gt 0 })
    if ($before.Count -ne 10 -or $after.Count -ne 10) { throw 'Incomplete engine measurements' }
    $bc=Median $before.create_ms; $ac=Median $after.create_ms; $br=Median $before.repair_ms; $ar=Median $after.repair_ms
    [ordered]@{Case=$case;BeforeCreateMs=$bc;AfterCreateMs=$ac;CreateReductionPercent=100*(1-$ac/$bc);
        BeforeRepairMs=$br;AfterRepairMs=$ar;RepairReductionPercent=100*(1-$ar/$br)}
}
$engine | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'engine-summary.json') -Encoding utf8
$report=@'
# Fastener 1.2.1：復旧処理の高速化

2026-09-26。Windows x86_64、15ワーカー。復旧データ作成・修復の処理を1.2.0と比較しました。通常の圧縮・解凍速度を示す測定ではありません。

## 変更

- 大きな片のReed–Solomon計算を、64 KiB区間ごとに並列化。既存ライブラリを使い、重ならない配列の部分参照で分割するため、大きなデータの複製は不要。
- 片のハッシュと大きな全体ハッシュ更新を並列化。再構成後の全片検証、全体ハッシュ、書庫形式検証、暗号認証は維持。
- 最大22 MiBの片バッファをグループ間で再利用。末尾のパディングを毎回ゼロ埋めし、修復後の書庫検証を始める前に解放。
- 256 KiB未満の片は逐次処理を維持。暗号化のパスワード導出条件は変更していない。

## 処理本体の比較

同じ `examples/recovery_benchmark.rs` を、配布済み1.2.0のソースと1.2.1のソースに対して別々にビルドしました。実行順を逆にした2回の比較で、各プロセスはウォームアップ1回＋5回測定、合計10回の中央値です。

プロセス起動、入力準備、外部の復元ハッシュ比較を計測から除外しています。実際のファイル入出力と、修復処理内の書庫検証は含みます。OSキャッシュは消去していません。入力は疑似乱数1 MiB／128 MiBから作成した非暗号化FSTです。

| 元データ | 作成 旧→新 ms | 作成時間短縮 | 修復 旧→新 ms | 修復時間短縮 |
|---|---:|---:|---:|---:|
'@
foreach ($row in $engine) {
    $name=if ($row.Case -eq 'large') { '128 MiB' } else { '1 MiB' }
    $report+="`n"+('| {0} | {1:F2} → {2:F2} | {3:F1}% | {4:F2} → {5:F2} | {6:F1}% |' -f $name,$row.BeforeCreateMs,$row.AfterCreateMs,$row.CreateReductionPercent,$row.BeforeRepairMs,$row.AfterRepairMs,$row.RepairReductionPercent)
}
$report+=@'


この割合は上記条件での処理本体の実測です。小容量では差が小さく、別のPCや媒体、暗号化書庫でも同じ割合になるとは限りません。

## 起動を含むCLI比較とメモリ

同じディレクトリの独立したEXEを交互に実行し、各1回ウォームアップ＋5回測定しました。出力は毎回別ファイルを使い、既存ファイルを連続置換する場合の一時的なアクセス拒否を避けています。CLIの小容量ケースにも大きな版間差が観測され、起動・実行環境の影響を切り離せないため、最適化の効果の説明には上の処理本体比較を使います。原因を特定したという主張はしていません。

メモリはプロセス実行中にWindowsのPeakWorkingSet64を5 ms間隔で観測した本測定の最大値です。終了直前のピークを逃す可能性があり、厳密な上限ではありません。

| ケース | 版 | 作成 ms | 修復 ms | 作成メモリ MiB | 修復メモリ MiB |
|---|---|---:|---:|---:|---:|
'@
foreach ($row in $cli) {
    $report+="`n"+('| {0} | {1} | {2:F2} | {3:F2} | {4:F2} | {5:F2} |' -f $row.Case,$row.Variant,$row.CreateMs,$row.RepairMs,$row.CreatePeakMiB,$row.RepairPeakMiB)
}
$report+=@'


tiny/small/large/encryptedは元データ48バイト／1 MiB／128 MiB／暗号化1 MiBです。

## 正確性と再現

CLI48回＋処理本体48回で復元を照合し、同じ入力に対する復旧ファイルも旧版と完全一致しました。逐次ライブラリ計算との一致、末尾が64 KiBに揃わない片、データ2片欠落、データ＋復旧片欠落、1ワーカープールでも追加検証しています。既存の破損・容量超過・誤パスワード・形式偽装・GUI経由の復旧テストも維持しています。最新のテスト件数は `validation.json` に記録しています。

生の測定は `measurements.json` と `engine-measurements.json`、バイナリと入力の識別情報は対応する `manifest.json` と `engine-manifest.json` にあります。

```powershell
# 同じディレクトリに旧版と新版を置く
./compare.ps1 -BeforeBinary ./bin/before.exe -AfterBinary ./bin/after.exe
# 同一のexampleを旧・新ソースに対してビルドし、bin/engine-before.exe と engine-after.exe に置く
./engine.ps1 -FixtureDirectory ./work-XXXXXXXX
./summarize.ps1
```

比較用EXE、参照ソースの展開コピー、測定入力は配布ZIPには含めません。書庫・復旧ファイルの形式と訂正能力は変更していません。
'@
Set-Content (Join-Path $PSScriptRoot 'REPORT.md') $report -Encoding utf8
$engine | ConvertTo-Json
