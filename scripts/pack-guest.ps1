# Build the desktop app in release and pack it for another PC on the network, for a two-PC test:
#   .\scripts\pack-guest.ps1
#   npx --yes http-server target\guest -p 8000      # then, on the other PC, http://<this PC>:8000/
# The debug app can't move: it loads its UI from this machine's dev server. The release build
# has the UI inside. This is not an installer (M10): the folder holds the app, the stream
# window and the DLLs they need, plus run.cmd, which keeps the app's log in desktop.log (the
# release app has no console). On the other PC: unblock the zip before extracting it (it came
# from a browser), allow the app on private networks when the firewall asks, and paste invites
# into the Join box (only debug builds register couchco:// links).

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'dev.ps1')

Push-Location (Join-Path $repo 'apps\desktop')
try {
    npx tauri build --no-bundle
    if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
} finally {
    Pop-Location
}
cargo build --release -p couch-viewer
if ($LASTEXITCODE -ne 0) { throw 'couch-viewer build failed' }

$release = Join-Path $repo 'target\release'
$out = Join-Path $repo 'target\guest'
$dir = Join-Path $out 'couch-coapp'
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force $dir | Out-Null

# The app links FFmpeg too (it encodes when hosting), so the DLLs go next to both exes.
Copy-Item (Join-Path $release 'couch-desktop.exe'), (Join-Path $release 'couch-viewer.exe') $dir
$dlls = Get-ChildItem (Join-Path $release '*.dll')
if ($dlls.Count -eq 0) { throw "No DLLs in $release; run 'cargo xtask stage-dlls'." }
$dlls | Copy-Item -Destination $dir

$run = @'
@echo off
rem Starts Couch Co-App with its log in desktop.log, next to this file.
rem The stream window logs to viewer.log in %LOCALAPPDATA%\CouchCoApp\profiles\<profile>.
set RUST_LOG=couch=debug,iroh=info
"%~dp0couch-desktop.exe" %* 2>> "%~dp0desktop.log"
'@
Set-Content -Path (Join-Path $dir 'run.cmd') -Value $run -Encoding ascii

$commit = git -C $repo rev-parse --short HEAD
$dirty = if (git -C $repo status --porcelain) { ' (with uncommitted changes)' } else { '' }
Set-Content -Path (Join-Path $dir 'BUILD.txt') -Value "Built from $commit$dirty on $(Get-Date -Format s)." -Encoding ascii

$zip = Join-Path $out 'couch-coapp.zip'
Compress-Archive -Path $dir -DestinationPath $zip
Write-Host "Packed $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB) from $commit$dirty."
Write-Host "Serve it with: npx --yes http-server target\guest -p 8000"
