param(
    [string]$InputPath = (Join-Path $PSScriptRoot 'work/mixed-2gib.bin'),
    [int]$Iterations = 4,
    [int]$Threads = 8
)
$ErrorActionPreference = 'Stop'
if ($Iterations -lt 1 -or $Threads -lt 1) { throw 'Iterations and Threads must be positive' }
$before = Join-Path $PSScriptRoot 'bin/before.exe'
$after = Join-Path $PSScriptRoot 'bin/after.exe'
foreach ($exe in @($before, $after)) {
    if (!(Test-Path -LiteralPath $exe)) { throw "Missing benchmark executable: $exe" }
}
$source = (Get-Item -LiteralPath $InputPath).FullName
$inputHash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
$work = Join-Path $PSScriptRoot ('work/run-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force $work | Out-Null

function Invoke-Measured([string]$Exe, [string[]]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new($Exe)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($start)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $timer.Stop()
    if ($process.ExitCode -ne 0) { throw "Execution failed: $($stderr.Result) $($stdout.Result)" }
    $process.Dispose()
    return $timer.Elapsed.TotalSeconds
}

$records = [Collections.Generic.List[object]]::new()
for ($iteration = 0; $iteration -le $Iterations; $iteration++) {
    $variants = if ($iteration % 2 -eq 0) { @('before', 'after') } else { @('after', 'before') }
    foreach ($variant in $variants) {
        $exe = if ($variant -eq 'before') { $before } else { $after }
        $archive = Join-Path $work "$variant-$iteration.fst"
        $restored = Join-Path $work "$variant-$iteration.restored"
        Write-Output "$variant iteration $($iteration): compressing"
        $compressSeconds = Invoke-Measured $exe @('--threads', "$Threads", 'compress', $source, '-o', $archive, '--level', '1')
        Write-Output "$variant iteration $($iteration): decompressing"
        $decompressSeconds = Invoke-Measured $exe @('--threads', "$Threads", 'decompress', $archive, '-o', $restored)
        $restoredHash = (Get-FileHash -LiteralPath $restored -Algorithm SHA256).Hash
        if ($restoredHash -ne $inputHash) { throw "Restored SHA-256 mismatch: $restored" }
        $records.Add([ordered]@{
            Variant = $variant; Iteration = $iteration; Warmup = ($iteration -eq 0)
            InputBytes = (Get-Item -LiteralPath $source).Length
            InputSHA256 = $inputHash; RestoredSHA256 = $restoredHash
            ArchiveBytes = (Get-Item -LiteralPath $archive).Length
            ArchiveSHA256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
            CompressSeconds = $compressSeconds; DecompressSeconds = $decompressSeconds
        })
        $records | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $PSScriptRoot 'measurements-reproduced.json') -Encoding utf8
        Remove-Item -LiteralPath $restored
        Write-Output "$variant iteration $($iteration): SHA-256 OK"
    }
}
$archiveHashes = @($records | ForEach-Object { $_.ArchiveSHA256 } | Select-Object -Unique)
if ($archiveHashes.Count -ne 1) { throw 'Old/new archive SHA-256 differs' }
[ordered]@{
    Date = (Get-Date -Format o); InputBytes = (Get-Item -LiteralPath $source).Length
    InputSHA256 = $inputHash; Threads = $Threads; Iterations = $Iterations
    BeforeExeSHA256 = (Get-FileHash -LiteralPath $before -Algorithm SHA256).Hash
    AfterExeSHA256 = (Get-FileHash -LiteralPath $after -Algorithm SHA256).Hash
    Method = 'Independent release binaries; process startup included; warmup excluded from medians; variants alternated; no cache flush; SHA-256 excluded from timing.'
} | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'manifest-reproduced.json') -Encoding utf8
