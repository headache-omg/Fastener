param(
    [Parameter(Mandatory)][string]$BeforeBinary,
    [string]$AfterBinary = (Join-Path $PSScriptRoot '../../target/release/fastener.exe'),
    [string]$InputFile = (Join-Path $PSScriptRoot '../debug-speed-2026-09-23/work/small.bin')
)
$ErrorActionPreference = 'Stop'
function Measure-Command([string]$exe, [string[]]$arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new((Resolve-Path $exe).Path)
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($start)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit(); $timer.Stop()
    try {
        if ($process.ExitCode -ne 0) { throw "Command failed: $($stderr.Result) $($stdout.Result)" }
        return $timer.Elapsed.TotalMilliseconds
    } finally { $process.Dispose() }
}
$work = Join-Path ([IO.Path]::GetTempPath()) ('fastener-small-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null
$archive = Join-Path $work 'archive.fst'; $restored = Join-Path $work 'restored'
$records = [Collections.Generic.List[object]]::new()
$inputHash = (Get-FileHash $InputFile).Hash
try {
    for ($iteration = 0; $iteration -le 5; $iteration++) {
        $order = if ($iteration % 2) { @('after','before') } else { @('before','after') }
        foreach ($variant in $order) {
            $exe = if ($variant -eq 'before') { $BeforeBinary } else { $AfterBinary }
            $compression = Measure-Command $exe @('--threads','15','compress',$InputFile,'-o',$archive,'--force','--chunk-size','65536')
            $decompression = Measure-Command $exe @('--threads','15','decompress',$archive,'-o',$restored,'--force')
            $verification = Measure-Command $exe @('--threads','15','verify',$archive)
            $restoredHash = (Get-FileHash $restored).Hash
            if ($inputHash -ne $restoredHash) { throw 'Restoration SHA256 mismatch' }
            $records.Add([ordered]@{Variant=$variant;Iteration=$iteration;CompressMs=$compression;
                DecompressMs=$decompression;VerifyMs=$verification;InputSha256=$inputHash;
                RestoredSha256=$restoredHash;ArchiveSha256=(Get-FileHash $archive).Hash;ArchiveBytes=(Get-Item $archive).Length})
        }
    }
} finally {
    foreach ($path in @($archive, $restored)) { if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path } }
    [IO.Directory]::Delete($work)
}
$records | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'small-input.json') -Encoding utf8
@{BeforeVersion=(& $BeforeBinary --version);AfterVersion=(& $AfterBinary --version);
    BeforeSha256=(Get-FileHash $BeforeBinary).Hash;AfterSha256=(Get-FileHash $AfterBinary).Hash;
    InputBytes=(Get-Item $InputFile).Length;InputSha256=$inputHash;TargetChunkBytes=65536;Threads=15;
    Method='Process startup and file IO included; 1 warmup then 5 runs per variant in alternating order; OS cache not cleared; SHA256 time excluded'} |
    ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'small-input-manifest.json') -Encoding utf8
$records | ForEach-Object { [pscustomobject]$_ } | Format-Table Variant,Iteration,CompressMs,DecompressMs
