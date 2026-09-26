$ErrorActionPreference = 'Stop'
$work = Join-Path $PSScriptRoot 'regression-work'
New-Item -ItemType Directory -Force $work | Out-Null
$inputFile = Join-Path $work 'input.bin'
$good = Join-Path $work 'good.fst'
$bad = Join-Path $work 'bad.fst'
$outputFile = Join-Path $work 'existing.bin'
[IO.File]::WriteAllBytes($inputFile, [byte[]](1..200))
$before = Join-Path $PSScriptRoot 'bin/before.exe'
& $before compress $inputFile -o $good --force | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Could not prepare regression fixture' }
$records = foreach ($offset in @(28, 80)) {
    $bytes = [IO.File]::ReadAllBytes($good)
    $bytes[$offset] = $bytes[$offset] -bxor 1
    [IO.File]::WriteAllBytes($bad, $bytes)
    foreach ($variant in @('before', 'after')) {
        $exe = Join-Path $PSScriptRoot "bin/$variant.exe"
        [IO.File]::WriteAllText($outputFile, 'keep')
        & $exe verify $bad 2>&1 | Out-Null
        $verifyCode = $LASTEXITCODE
        & $exe decompress $bad -o $outputFile --force 2>&1 | Out-Null
        $decodeCode = $LASTEXITCODE
        $preserved = (Test-Path $outputFile) -and ([IO.File]::ReadAllText($outputFile) -eq 'keep')
        if ($variant -eq 'after' -and ($verifyCode -eq 0 -or $decodeCode -eq 0 -or !$preserved)) {
            throw 'Regression still present'
        }
        [ordered]@{
            Variant = $variant
            Corruption = $(if ($offset -eq 28) { 'whole-file hash' } else { 'chunk hash' })
            VerifyExit = $verifyCode; DecompressExit = $decodeCode; ExistingOutputPreserved = $preserved
        }
    }
}
$records | ConvertTo-Json | Set-Content (Join-Path $PSScriptRoot 'regressions.json') -Encoding utf8
$records | ForEach-Object { [pscustomobject]$_ } | Format-Table
