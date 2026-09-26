param([string]$OutputDirectory = (Split-Path $PSScriptRoot -Parent))
$ErrorActionPreference = 'Stop'
$versionMatch = [regex]::Match((Get-Content (Join-Path $PSScriptRoot 'Cargo.toml') -Raw), '(?m)^version = "([0-9.]+)"')
if (!$versionMatch.Success) { throw 'Package version not found' }
$version = $versionMatch.Groups[1].Value
$stage = Join-Path $PSScriptRoot ("release/oss-$version-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$source = Join-Path $stage 'source'
$windows = Join-Path $stage 'windows'
New-Item -ItemType Directory -Force $source, $OutputDirectory | Out-Null
foreach ($name in @('src', 'tests', 'examples', 'samples', 'docs', 'LICENSES', 'Cargo.toml', 'Cargo.lock', 'README.md', 'FORMAT.md', 'LICENSE', 'CHANGELOG.md', '.gitignore', 'build-release.ps1', 'package-oss.ps1')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $source -Recurse
}
$benchDest = Join-Path $source 'benchmarks'
New-Item -ItemType Directory $benchDest | Out-Null
Get-ChildItem (Join-Path $PSScriptRoot 'benchmarks') -File | Copy-Item -Destination $benchDest
$comparison = 'benchmarks/debug-speed-2026-09-23'
$comparisonDest = Join-Path $source $comparison
New-Item -ItemType Directory $comparisonDest | Out-Null
Get-ChildItem (Join-Path $PSScriptRoot $comparison) -File | Copy-Item -Destination $comparisonDest
Copy-Item -LiteralPath (Join-Path $PSScriptRoot "$comparison/before") -Destination $comparisonDest -Recurse
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'benchmarks/v1.1.0') -Destination $benchDest -Recurse
$latestBench = Join-Path $benchDest 'v1.1.1'
New-Item -ItemType Directory $latestBench | Out-Null
Get-ChildItem (Join-Path $PSScriptRoot 'benchmarks/v1.1.1') -File | Copy-Item -Destination $latestBench
$recoveryBench = Join-Path $benchDest 'v1.2.0'
New-Item -ItemType Directory $recoveryBench | Out-Null
Get-ChildItem (Join-Path $PSScriptRoot 'benchmarks/v1.2.0') -File | Copy-Item -Destination $recoveryBench
$parallelBench = Join-Path $benchDest 'v1.2.1'
New-Item -ItemType Directory $parallelBench | Out-Null
Get-ChildItem (Join-Path $PSScriptRoot 'benchmarks/v1.2.1') -File | Copy-Item -Destination $parallelBench
$largeBench = Join-Path $benchDest 'v1.2.2'
New-Item -ItemType Directory $largeBench | Out-Null
Get-ChildItem (Join-Path $PSScriptRoot 'benchmarks/v1.2.2') -File | Copy-Item -Destination $largeBench
$extractionBench = Join-Path $benchDest 'v1.2.3'
New-Item -ItemType Directory $extractionBench | Out-Null
Get-ChildItem (Join-Path $PSScriptRoot 'benchmarks/v1.2.3') -File | Copy-Item -Destination $extractionBench
Copy-Item -LiteralPath $source -Destination $windows -Recurse
New-Item -ItemType Directory (Join-Path $windows 'bin') | Out-Null
foreach ($exe in @('fastener.exe', 'fastener-gui.exe')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "target/release/$exe") -Destination (Join-Path $windows 'bin')
}
$cli = Join-Path $windows 'bin/fastener.exe'
$reportedVersion = & $cli --version
if ($LASTEXITCODE -ne 0 -or $reportedVersion -ne "fastener $version") { throw 'Build the current release before packaging' }
foreach ($kind in @('source', 'windows')) {
    $tree = Join-Path $stage $kind
    $files = @(Get-ChildItem $tree -Recurse -File -Force)
    foreach ($file in $files) {
        $bytes = [IO.File]::ReadAllBytes($file.FullName)
        foreach ($encoding in @([Text.Encoding]::UTF8, [Text.Encoding]::Unicode)) {
            $text = $encoding.GetString($bytes)
            if (($env:USERPROFILE -and $text.Contains($env:USERPROFILE)) -or ($env:USERNAME -and $text.Contains($env:USERNAME))) {
                throw "Personal path/name found in $($file.Name); use build-release.ps1"
            }
        }
    }
    $sums = foreach ($file in ($files | Sort-Object FullName)) {
        $relative = [IO.Path]::GetRelativePath($tree, $file.FullName).Replace('\', '/')
        '{0}  {1}' -f (Get-FileHash $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $relative
    }
    $sums | Set-Content (Join-Path $tree 'SHA256SUMS.txt') -Encoding utf8
    $suffix = if ($kind -eq 'source') { 'source' } else { 'windows-x86_64' }
    $zipPath = Join-Path $OutputDirectory "fastener-oss-v$version-$suffix.zip"
    if (Test-Path $zipPath) { throw "Output already exists: $zipPath" }
    Compress-Archive -Path (Join-Path $tree '*') -DestinationPath $zipPath
    $archive = [IO.Compression.ZipFile]::OpenRead($zipPath)
    $verified = 0
    try {
        foreach ($entry in $archive.Entries) {
            if (!$entry.Name) { continue }
            $stream = $entry.Open()
            $sha = [Security.Cryptography.SHA256]::Create()
            try {
                $actual = [Convert]::ToHexString($sha.ComputeHash($stream))
                $expected = (Get-FileHash (Join-Path $tree $entry.FullName)).Hash
                if ($actual -ne $expected) { throw "ZIP entry mismatch: $($entry.FullName)" }
                $verified++
            } finally { $stream.Dispose(); $sha.Dispose() }
        }
        if ($verified -ne $files.Count + 1) { throw 'ZIP file count mismatch' }
    } finally { $archive.Dispose() }
    [pscustomobject]@{ File = $zipPath; VerifiedFiles = $verified; SHA256 = (Get-FileHash $zipPath).Hash }
}
