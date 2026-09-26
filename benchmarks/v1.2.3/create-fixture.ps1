param(
    [ValidateSet(2, 50)][int]$SizeGiB = 50,
    [string]$Seed = (Join-Path $PSScriptRoot '../v1.2.2/work/mixed-512.bin')
)
$ErrorActionPreference = 'Stop'
$work = Join-Path $PSScriptRoot 'work'
New-Item -ItemType Directory -Force $work | Out-Null
$destination = Join-Path $work "mixed-${SizeGiB}gib.bin"
if (!(Test-Path -LiteralPath $Seed)) {
    New-Item -ItemType Directory -Force (Split-Path $Seed -Parent) | Out-Null
    Add-Type -TypeDefinition @'
using System;
using System.IO;
public static class FastenerExtractionFixture {
    public static void WriteMixed256(string path) {
        uint state = 0x12345678;
        byte[] block = new byte[1024 * 1024];
        using (var file = File.Create(path)) {
            for (int region = 0; region < 256; region++) {
                for (int i = 0; i < block.Length; i++) {
                    if (region % 4 == 3) {
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
    $seed256 = Join-Path $work 'mixed-256-seed.bin'
    [FastenerExtractionFixture]::WriteMixed256($seed256)
    $seedStream = [IO.File]::OpenRead($seed256)
    $seedOutput = [IO.File]::Create($Seed)
    try {
        for ($copy = 1; $copy -le 2; $copy++) {
            $seedStream.Position = 0
            $seedStream.CopyTo($seedOutput, 4 * 1MB)
        }
    } finally {
        $seedOutput.Dispose()
        $seedStream.Dispose()
    }
    Remove-Item -LiteralPath $seed256
}
$seedFile = Get-Item -LiteralPath $Seed
if ($seedFile.Length -ne 512 * 1MB) { throw 'Expected a 512 MiB seed file' }
$expectedSize = [long]$SizeGiB * 1GB
if (Test-Path -LiteralPath $destination) {
    if ((Get-Item -LiteralPath $destination).Length -ne $expectedSize) {
        throw 'Existing fixture has the wrong length; inspect it before retrying'
    }
    Write-Output "Fixture already exists: $destination"
    return
}
if ((Get-PSDrive C).Free -lt ([long]$SizeGiB * 3 + 5) * 1GB) { throw 'Insufficient free space for benchmark input, archive, and restored file' }
$source = [IO.File]::OpenRead($seedFile.FullName)
$output = [IO.File]::Open($destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
try {
    $copies = $SizeGiB * 2
    for ($copy = 1; $copy -le $copies; $copy++) {
        $source.Position = 0
        $source.CopyTo($output, 4 * 1MB)
        if ($copy % 10 -eq 0 -or $copy -eq $copies) {
            Write-Output ('{0}/{1} copies: {2:N1} GiB written; {3:N1} GiB free' -f $copy, $copies, ($output.Position / 1GB), ((Get-PSDrive C).Free / 1GB))
        }
    }
} finally {
    $output.Dispose()
    $source.Dispose()
}
if ((Get-Item -LiteralPath $destination).Length -ne $expectedSize) { throw 'Fixture length mismatch' }
Write-Output "Created $destination ($expectedSize bytes)"
