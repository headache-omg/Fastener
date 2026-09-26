param([Parameter(Mandatory)][string]$BeforeBinary, [string]$AfterBinary = (Join-Path $PSScriptRoot "../../target/release/fastener.exe"), [int]$Iterations = 5, [int]$Threads = 15)
$ErrorActionPreference = 'Stop'
if ($Iterations -lt 1 -or $Threads -lt 1) { throw 'Iterations and Threads must be positive' }
$root = $PSScriptRoot
$work = Join-Path $root 'work'
New-Item -ItemType Directory -Force $work | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.IO;
public static class FastenerFixture {
    public static void Write(string path, int mib, bool randomOnly) {
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
}
'@
[FastenerFixture]::Write((Join-Path $work 'mixed.bin'), 256, $false)
[FastenerFixture]::Write((Join-Path $work 'random.bin'), 128, $true)
[FastenerFixture]::Write((Join-Path $work 'small.bin'), 1, $false)
$cases = @(
    @{ Name = 'mixed-balanced'; File = 'mixed.bin'; Level = 1 },
    @{ Name = 'mixed-fast'; File = 'mixed.bin'; Level = 0 },
    @{ Name = 'random-balanced'; File = 'random.bin'; Level = 1 },
    @{ Name = 'small-balanced'; File = 'small.bin'; Level = 1 }
)
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
    $result = @{ Ms = $timer.Elapsed.TotalMilliseconds; ExitCode = $process.ExitCode; Error = $stderr.Result }
    if ($process.ExitCode -ne 0) { throw "Execution failed: $($stderr.Result) $($stdout.Result)" }
    $process.Dispose()
    return $result.Ms
}
$records = [Collections.Generic.List[object]]::new()
foreach ($case in $cases) {
    $inputFile = Join-Path $work $case.File
    $inputHash = (Get-FileHash $inputFile -Algorithm SHA256).Hash
    for ($iteration = 0; $iteration -le $Iterations; $iteration++) {
        $variants = if ($iteration % 2 -eq 0) { @('before', 'after') } else { @('after', 'before') }
        foreach ($variant in $variants) {
            $exe = (Resolve-Path $(if ($variant -eq "before") { $BeforeBinary } else { $AfterBinary })).Path
            $archive = Join-Path $work "$variant.fst"
            $restored = Join-Path $work "$variant.restored"
            $compressMs = Invoke-Measured $exe @('--threads', "$Threads", 'compress', $inputFile, '-o', $archive, '--level', "$($case.Level)", '--force')
            $decompressMs = Invoke-Measured $exe @('--threads', "$Threads", 'decompress', $archive, '-o', $restored, '--force')
            $verifyMs = Invoke-Measured $exe @('--threads', "$Threads", 'verify', $archive)
            $restoredHash = (Get-FileHash $restored -Algorithm SHA256).Hash
            if ($restoredHash -ne $inputHash) { throw 'Restored SHA256 mismatch' }
            $record = [ordered]@{
                Case = $case.Name; Variant = $variant; Iteration = $iteration; Warmup = ($iteration -eq 0)
                InputBytes = (Get-Item $inputFile).Length; ArchiveBytes = (Get-Item $archive).Length
                InputSha256 = $inputHash; RestoredSha256 = $restoredHash
                ArchiveSha256 = (Get-FileHash $archive -Algorithm SHA256).Hash
                CompressMs = $compressMs; DecompressMs = $decompressMs; VerifyMs = $verifyMs
            }
            $records.Add($record)
            $records | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $root 'measurements.json') -Encoding utf8
            Write-Output ("{0} {1} run={2}: compress={3:N1}ms decompress={4:N1}ms verify={5:N1}ms SHA256=OK" -f $case.Name, $variant, $iteration, $compressMs, $decompressMs, $verifyMs)
        }
    }
}
$manifest = [ordered]@{
    Date = (Get-Date -Format o); Threads = $Threads; Iterations = $Iterations
    LogicalProcessors = [Environment]::ProcessorCount; OS = [Environment]::OSVersion.VersionString
    BeforeSha256 = (Get-FileHash $BeforeBinary).Hash
    AfterSha256 = (Get-FileHash $AfterBinary).Hash
    Method = 'Independent release binaries; process startup included; one warmup; alternating order; cached filesystem; SHA256 checks excluded from timing; no cache flush.'
}
$manifest | ConvertTo-Json | Set-Content (Join-Path $root 'manifest.json') -Encoding utf8

