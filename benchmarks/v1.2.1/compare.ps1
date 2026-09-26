param([string]$BeforeBinary = (Join-Path $PSScriptRoot 'bin/before.exe'), [string]$AfterBinary = (Join-Path $PSScriptRoot 'bin/after.exe'), [int]$Iterations = 5)
$ErrorActionPreference = 'Stop'
if ($Iterations -lt 1) { throw 'Iterations must be positive' }
$exe = (Resolve-Path $BeforeBinary).Path
$work = Join-Path $PSScriptRoot ('work-' + [guid]::NewGuid().ToString('N').Substring(0,8))
New-Item -ItemType Directory $work | Out-Null
function Invoke-Measured([string[]]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new($exe)
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($start)
    $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
    $peak = 0L
    while (!$process.WaitForExit(5)) {
        try { $process.Refresh(); $peak = [Math]::Max($peak, $process.PeakWorkingSet64) } catch {}
    }
    $timer.Stop()
    try {
        if ($process.ExitCode -ne 0) { throw "Command failed: $($stdout.Result) $($stderr.Result)" }
        if ($peak -eq 0) { throw 'No working-set observation was available' }
        [pscustomobject]@{Ms=$timer.Elapsed.TotalMilliseconds;PeakWorkingSetBytesObserved=$peak}
    } finally { $process.Dispose() }
}
Add-Type -TypeDefinition @'
using System.IO;
public static class RecoveryFixture {
    public static void Write(string path, int length) {
        uint state = 0x87654321;
        byte[] buffer = new byte[1048576];
        using (var file = File.Create(path)) {
            for (int offset = 0; offset < length;) {
                int count = System.Math.Min(buffer.Length, length-offset);
                for (int i = 0; i < count; i++) {
                    state ^= state << 13; state ^= state >> 17; state ^= state << 5;
                    buffer[i] = (byte)state;
                }
                file.Write(buffer, 0, count); offset += count;
            }
        }
    }
}
'@
$secret = Join-Path $work 'test-password.txt'
[IO.File]::WriteAllText($secret, 'recovery-benchmark-only', [Text.UTF8Encoding]::new($false))
$records = [Collections.Generic.List[object]]::new()
try {
    foreach ($case in @(
        @{Name='tiny';Bytes=48;Encrypted=$false},
        @{Name='small';Bytes=1MB;Encrypted=$false},
        @{Name='large';Bytes=128MB;Encrypted=$false},
        @{Name='encrypted';Bytes=1MB;Encrypted=$true}
    )) {
        $source = Join-Path $work "$($case.Name).bin"
        $archive = Join-Path $work "$($case.Name).fst"
        $sidecar = "$archive.par"
        $damaged = Join-Path $work "$($case.Name).damaged.fst"
        $restored = Join-Path $work "$($case.Name).repaired.fst"
        [RecoveryFixture]::Write($source, $case.Bytes)
        $arguments = @('--threads','15','compress',$source,'-o',$archive)
        if ($case.Encrypted) { $arguments += @('--encrypt','--password-file',$secret) }
        Invoke-Measured $arguments | Out-Null
        $archiveHash = (Get-FileHash $archive).Hash
        for ($iteration = 0; $iteration -le $Iterations; $iteration++) {
            $order = if ($iteration % 2) { @('after','before') } else { @('before','after') }
            foreach ($variant in $order) {
            $exe = (Resolve-Path $(if ($variant -eq 'before') { $BeforeBinary } else { $AfterBinary })).Path
            $sidecar = Join-Path $work "$($case.Name)-$variant-$iteration.par"
            $damaged = Join-Path $work "$($case.Name)-$variant-$iteration.damaged.fst"
            $restored = Join-Path $work "$($case.Name)-$variant-$iteration.repaired.fst"
            $creation = Invoke-Measured @('--threads','15','recovery-create',$archive,'-o',$sidecar)
            $stream = [IO.File]::OpenRead($sidecar)
            try { $header = [byte[]]::new(16); $stream.ReadExactly($header); $shard = [BitConverter]::ToUInt32($header,12) } finally { $stream.Dispose() }
            Copy-Item -LiteralPath $archive -Destination $damaged
            $stream = [IO.File]::Open($damaged,[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite)
            try {
                foreach ($offset in @(0L,([long]$shard+31))) {
                    if ($offset -ge $stream.Length) { continue }
                    $stream.Position = $offset; $value = $stream.ReadByte()
                    $stream.Position = $offset; $stream.WriteByte([byte]($value -bxor 128))
                }
            } finally { $stream.Dispose() }
            $arguments = @('--threads','15','repair',$damaged,'--recovery',$sidecar,'-o',$restored,'--force')
            if ($case.Encrypted) { $arguments += @('--password-file',$secret) }
            $repair = Invoke-Measured $arguments
            $restoredHash = (Get-FileHash $restored).Hash
            if ($restoredHash -ne $archiveHash) { throw 'Repaired archive SHA256 mismatch' }
            $records.Add([ordered]@{Variant=$variant;RecoverySha256=(Get-FileHash $sidecar).Hash;Case=$case.Name;Iteration=$iteration;Warmup=($iteration -eq 0);
                ArchiveBytes=(Get-Item $archive).Length;RecoveryBytes=(Get-Item $sidecar).Length;
                ShardBytes=$shard;CreateMs=$creation.Ms;RepairMs=$repair.Ms;
                CreatePeakWorkingSetBytesObserved=$creation.PeakWorkingSetBytesObserved;
                RepairPeakWorkingSetBytesObserved=$repair.PeakWorkingSetBytesObserved;
                ArchiveSha256=$archiveHash;RestoredSha256=$restoredHash})
            $records | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'measurements.json') -Encoding utf8
            Write-Output ("{0} $variant run={1}: create={2:N1}ms repair={3:N1}ms SHA256=OK" -f $case.Name,$iteration,$creation.Ms,$repair.Ms)
        }
        }
    }
} finally { if (Test-Path -LiteralPath $secret) { Remove-Item -LiteralPath $secret } }
@{BeforeVersion=(& $BeforeBinary --version);AfterVersion=(& $AfterBinary --version);BeforeSha256=(Get-FileHash $BeforeBinary).Hash;AfterSha256=(Get-FileHash $AfterBinary).Hash;Iterations=$Iterations;Threads=15;
    OS=[Environment]::OSVersion.VersionString;LogicalProcessors=[Environment]::ProcessorCount;
    Method='Same directory, alternating independent versions, one warmup + measured runs; process startup and IO included; cache not cleared; SHA256 excluded. Windows PeakWorkingSet64 sampled every 5ms while process runs; may miss the final peak. Repair includes archive verification and encrypted authentication.'} |
    ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'manifest.json') -Encoding utf8


