# Prepare a PowerShell session for development. Dot-source it from the repo root:
#   . .\scripts\dev.ps1
# Rebuilds PATH from the registry (so tools installed after the terminal opened are found)
# and puts the FFmpeg and SDL3 DLLs on it. On a GitHub Actions runner (the release workflow,
# M26c) the PATH the job's setup steps made is kept instead: the registry doesn't have it.

$repo = Split-Path -Parent $PSScriptRoot
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$rest = if ($env:GITHUB_ACTIONS -eq 'true') { @($env:Path) } else { @($machinePath, $userPath) }
$env:Path = @(
    (Join-Path $repo 'third_party\ffmpeg\bin'),
    (Join-Path $repo 'third_party\sdl3\lib\x64'),
    (Join-Path $env:USERPROFILE '.cargo\bin')
) + $rest -join ';'

if (-not $env:RUST_LOG) { $env:RUST_LOG = 'couch=debug,iroh=info' }
