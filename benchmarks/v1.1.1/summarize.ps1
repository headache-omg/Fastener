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
$summary | ForEach-Object { [pscustomobject]$_ } | Format-Table
