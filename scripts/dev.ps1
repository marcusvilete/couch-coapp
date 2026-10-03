# Prepare a PowerShell session for development. Dot-source it from the repo root:
#   . .\scripts\dev.ps1
# Rebuilds PATH from the registry (so tools installed after the terminal opened are found)
# and puts the FFmpeg and SDL3 DLLs on it.

$repo = Split-Path -Parent $PSScriptRoot
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$env:Path = @(
    (Join-Path $repo 'third_party\ffmpeg\bin'),
    (Join-Path $repo 'third_party\sdl3\lib\x64'),
    (Join-Path $env:USERPROFILE '.cargo\bin'),
    $machinePath,
    $userPath
) -join ';'

if (-not $env:RUST_LOG) { $env:RUST_LOG = 'couch=debug,iroh=info' }
