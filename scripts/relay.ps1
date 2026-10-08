# Runs our relay on this PC for testing (M7): the official iroh-relay server that
# `cargo xtask relay-tool` builds, in its --dev mode (plain HTTP on port 3340), with
# production's limits from deploy\relay\dev.toml and the token from release.toml.
#   .\scripts\relay.ps1          # this PC only: http://localhost:3340
#   .\scripts\relay.ps1 -Lan     # also your LAN, for your other PC (Windows Firewall asks once)
#   .\scripts\relay.ps1 -Open    # no token: any client may use it
# Ctrl+C stops it. Metrics: http://127.0.0.1:9090/metrics

param(
    [switch] $Lan,
    [switch] $Open
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'read-release-config.ps1')

$exe = Join-Path $repo 'target\tools\bin\iroh-relay.exe'
if (-not (Test-Path $exe)) {
    throw "No relay server built yet: run 'cargo xtask relay-tool' first."
}

$text = Get-Content (Join-Path $repo 'deploy\relay\dev.toml') -Raw
if ($Lan) {
    $text = $text -replace 'http_bind_addr = "127.0.0.1:3340"', 'http_bind_addr = "0.0.0.0:3340"'
}
if ($Open) {
    $text = $text -replace '(?m)^access\.shared_token = .*$', 'access = "everyone"'
    Remove-Item Env:IROH_RELAY_ACCESS_TOKEN -ErrorAction SilentlyContinue
} else {
    $path = Get-ReleaseConfigPath
    if (-not (Test-Path $path)) {
        throw "No release.toml at $path for the token: run 'cargo xtask release-config', or pass -Open."
    }
    $config = Read-ReleaseConfig $path
    if (-not $config.relay_token) { throw "$path has no relay_token." }
    $env:IROH_RELAY_ACCESS_TOKEN = $config.relay_token
}
$config_file = Join-Path $repo 'target\tools\relay-dev.toml'
# Without a byte-order mark, which Windows PowerShell's UTF8 would add and TOML doesn't take.
[System.IO.File]::WriteAllText($config_file, $text, (New-Object System.Text.UTF8Encoding $false))

Write-Host 'Our relay, on this PC:'
Write-Host '  this PC:  http://localhost:3340'
if ($Lan) {
    $ips = Get-NetIPAddress -AddressFamily IPv4 -PrefixOrigin Dhcp, Manual -ErrorAction SilentlyContinue |
        Where-Object { $_.IPAddress -match '^(10\.|172\.(1[6-9]|2\d|3[01])\.|192\.168\.)' } |
        ForEach-Object { $_.IPAddress }
    foreach ($ip in $ips) { Write-Host "  your LAN: http://${ip}:3340" }
}
if ($Open) {
    Write-Host '  no token: any client may use it'
} else {
    Write-Host '  token:    the relay_token in release.toml (couch-cli: --relay-token)'
}
Write-Host ''

if (-not $env:RUST_LOG) { $env:RUST_LOG = 'info' }
& $exe --dev --config-path $config_file
