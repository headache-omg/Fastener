param([switch]$Offline)
$ErrorActionPreference = 'Stop'
$previousFlags = $env:CARGO_ENCODED_RUSTFLAGS
Push-Location $PSScriptRoot
try {
    # Remap build paths in all Rust crates, including dependencies.
    $remap = "--remap-path-prefix=$env:USERPROFILE=/build"
    $env:CARGO_ENCODED_RUSTFLAGS = if ($previousFlags) {
        $previousFlags + [char]31 + $remap
    } else { $remap }
    $arguments = @('build', '--release', '--locked')
    if ($Offline) { $arguments += '--offline' }
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $previousFlags
    Pop-Location
}
