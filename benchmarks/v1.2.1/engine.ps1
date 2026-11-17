param([Parameter(Mandatory)][string]$FixtureDirectory)
$ErrorActionPreference = 'Stop'
$records = [Collections.Generic.List[object]]::new()
foreach ($case in @('small','large')) {
    $inputFile = Join-Path $FixtureDirectory "$case.fst"
    foreach ($pass in 0..1) {
        $order = if ($pass) { @('after','before') } else { @('before','after') }
        foreach ($variant in $order) {
            $exe = Join-Path $PSScriptRoot "bin/engine-$variant.exe"
            $output = & $exe $inputFile $FixtureDirectory 5
            if ($LASTEXITCODE -ne 0) { throw 'Engine benchmark failed' }
            foreach ($line in $output) {
                $row = $line | ConvertFrom-Json
                $expectedVersion = if ($variant -eq 'before') { '1.2.0' } else { '1.2.1' }
                if ($row.version -ne $expectedVersion) { throw 'Wrong benchmark version' }
                $row | Add-Member Case $case
                $row | Add-Member Variant $variant
                $row | Add-Member Pass $pass
                $records.Add($row)
            }
            $records | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'engine-measurements.json') -Encoding utf8
            Write-Output "$case $variant pass=$pass completed; archive hashes verified"
        }
    }
}
foreach ($group in ($records | Group-Object Case)) {
    if (@($group.Group.parity_blake3 | Sort-Object -Unique).Count -ne 1) { throw 'Recovery bytes differ across versions' }
    if (@($group.Group.archive_blake3 | Sort-Object -Unique).Count -ne 1) { throw 'Fixture mismatch' }
}
@{BeforeSha256=(Get-FileHash (Join-Path $PSScriptRoot 'bin/engine-before.exe')).Hash;
    AfterSha256=(Get-FileHash (Join-Path $PSScriptRoot 'bin/engine-after.exe')).Hash;
    ExampleSha256=(Get-FileHash (Join-Path $PSScriptRoot '../../examples/recovery_benchmark.rs')).Hash;
    Fixtures=@(foreach ($case in @('small','large')) { @{Name="$case.fst";Sha256=(Get-FileHash (Join-Path $FixtureDirectory "$case.fst")).Hash} });
    Method='Same example compiled with original 1.2.0 source and current 1.2.1. Two reversed-order passes; one warmup then five samples per process. Startup, fixture preparation and external hash comparisons excluded; actual file IO and internal verification included. 15 workers. Cache not cleared.'} |
    ConvertTo-Json -Depth 5 | Set-Content (Join-Path $PSScriptRoot 'engine-manifest.json') -Encoding utf8
