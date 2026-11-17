param([int]$Iterations = 4, [int]$Threads = 8)
$ErrorActionPreference = 'Stop'
if ($Iterations -lt 1 -or $Threads -lt 1) { throw 'Iterations and Threads must be positive' }
$root = $PSScriptRoot
$work = Join-Path $root 'work'
New-Item -ItemType Directory -Force $work | Out-Null

# Recreate the prior benchmark's deterministic 128/256 MiB fixtures, then
# repeat them to 512 MiB. Each variant receives exactly the same input bytes.
if (!(Test-Path (Join-Path $work 'random-512.bin')) -or !(Test-Path (Join-Path $work 'mixed-512.bin'))) {
    Add-Type -TypeDefinition @'
using System;
using System.IO;
public static class FastenerLargeFixture {
    public static void WriteSeed(string path, int mib, bool randomOnly) {
        uint state = 0x12345678;
        byte[] block = new byte[1024 * 1024];
        using (var file = File.Create(path)) {
            for (int region = 0; region < mib; region++) {
                for (int i = 0; i < block.Length; i++) {
                    if (randomOnly || region % 4 == 3) {
                        state ^= state << 13; state ^= state >> 17; state ^= state << 5;
                        block[i] = (byte)state;
                    } else { block[i] = region % 4 == 2 ? (byte)0 : (byte)(i % (region % 4 == 0 ? 37 : 251)); }
                }
                file.Write(block, 0, block.Length);
            }
        }
    }
    public static void Repeat(string source, string destination, int count) {
        using (var input = File.OpenRead(source))
        using (var output = File.Create(destination)) {
            for (int i = 0; i < count; i++) { input.Position = 0; input.CopyTo(output); }
        }
    }
}
'@
    foreach ($item in @(@('random', 128, 4, $true), @('mixed', 256, 2, $false))) {
        $seed = Join-Path $work "$($item[0])-seed.bin"
        $fixture = Join-Path $work "$($item[0])-512.bin"
        [FastenerLargeFixture]::WriteSeed($seed, $item[1], $item[3])
        [FastenerLargeFixture]::Repeat($seed, $fixture, $item[2])
        Remove-Item -LiteralPath $seed
    }
}

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
    return $timer.Elapsed.TotalMilliseconds
}

$records = [Collections.Generic.List[object]]::new()
foreach ($case in @('random', 'mixed')) {
    $sourcePath = Join-Path $work "$case-512.bin"
    $inputHash = (Get-FileHash $sourcePath -Algorithm SHA256).Hash
    for ($iteration = 0; $iteration -le $Iterations; $iteration++) {
        $variants = if ($iteration % 2 -eq 0) { @('before', 'after') } else { @('after', 'before') }
        foreach ($variant in $variants) {
            $exe = Join-Path $root "bin/$variant.exe"
            $archive = Join-Path $work "$case-$variant-$iteration.fst"
            $restored = Join-Path $work "$case-$variant-$iteration.restored"
            $compressMs = Invoke-Measured $exe @('--threads', "$Threads", 'compress', $sourcePath, '-o', $archive, '--level', '1')
            $decompressMs = Invoke-Measured $exe @('--threads', "$Threads", 'decompress', $archive, '-o', $restored)
            $verifyMs = Invoke-Measured $exe @('--threads', "$Threads", 'verify', $archive)
            $restoredHash = (Get-FileHash $restored -Algorithm SHA256).Hash
            if ($restoredHash -ne $inputHash) { throw 'Restored SHA-256 mismatch' }
            $record = [ordered]@{
                Case = $case; Variant = $variant; Iteration = $iteration; Warmup = ($iteration -eq 0)
                InputBytes = (Get-Item $sourcePath).Length; ArchiveBytes = (Get-Item $archive).Length
                InputSha256 = $inputHash; RestoredSha256 = $restoredHash
                ArchiveSha256 = (Get-FileHash $archive -Algorithm SHA256).Hash
                CompressMs = $compressMs; DecompressMs = $decompressMs; VerifyMs = $verifyMs
            }
            $records.Add($record)
            $records | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $root 'measurements.json') -Encoding utf8
            Write-Output ("{0} {1} run={2}: compress={3:N1}ms decompress={4:N1}ms verify={5:N1}ms SHA256=OK" -f $case, $variant, $iteration, $compressMs, $decompressMs, $verifyMs)
            Remove-Item -LiteralPath $restored, $archive
        }
    }
}
$manifest = [ordered]@{
    Date = (Get-Date -Format o); Threads = $Threads; Iterations = $Iterations
    LogicalProcessors = [Environment]::ProcessorCount; OS = [Environment]::OSVersion.VersionString
    BeforeSha256 = (Get-FileHash (Join-Path $root 'bin/before.exe')).Hash
    AfterSha256 = (Get-FileHash (Join-Path $root 'bin/after.exe')).Hash
    Method = 'Independent release binaries; process startup included; one warmup; alternating order; cached filesystem; SHA-256 excluded from timing; no cache flush.'
}
$manifest | ConvertTo-Json | Set-Content (Join-Path $root 'manifest.json') -Encoding utf8
