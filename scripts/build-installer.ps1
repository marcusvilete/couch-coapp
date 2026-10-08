# Builds the Windows installer (M10a) for another PC:
#   .\scripts\build-installer.ps1
#   npx --yes http-server target\installer -p 8000      # then, on the other PC, http://<this PC>:8000/
# It asks to install for "just me" (%LOCALAPPDATA%\Programs) or everyone (Program Files; on an
# administrator account Windows asks for administrator rights when it opens), registers couchco:// links,
# and after installing offers ViGEmBus and HidHide (their own setups, which ask for
# administrator rights). Unsigned until M10b: Windows SmartScreen warns, and "More info" then
# "Run anyway" installs it. Logs go to desktop.log in %LOCALAPPDATA%\CouchCoApp\profiles\<profile>.

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'dev.ps1')

# The pinned DLLs and driver installers, next to the release build, where the bundle takes them.
cargo xtask fetch-deps
if ($LASTEXITCODE -ne 0) { throw 'fetch-deps failed' }
cargo xtask stage-dlls
if ($LASTEXITCODE -ne 0) { throw 'stage-dlls failed' }

# One version everywhere: the installer shows tauri.conf.json's, other members see Cargo's.
$cargo = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
$tauri = (Get-Content (Join-Path $repo 'apps\desktop\src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json).version
$npm = (Get-Content (Join-Path $repo 'apps\desktop\package.json') -Raw | ConvertFrom-Json).version
if ($cargo -ne $tauri -or $cargo -ne $npm) {
    throw "Versions differ: Cargo.toml $cargo, tauri.conf.json $tauri, package.json $npm. Make them the same."
}

Push-Location (Join-Path $repo 'apps\desktop')
try {
    # The installer's own settings (the bundled files, the driver offers) live apart from
    # tauri.conf.json: Tauri's build step copies bundled files next to the app it builds,
    # which in development would overwrite the debug stream window with the release one.
    # Its beforeBuildCommand builds couch-viewer (release) and the front end first.
    npx tauri build --config src-tauri/tauri.installer.conf.json
    if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
} finally {
    Pop-Location
}

$built = Get-ChildItem (Join-Path $repo 'target\release\bundle\nsis') -Filter "*_$($cargo)_x64-setup.exe" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $built) { throw 'No installer was produced.' }

$commit = git -C $repo rev-parse --short HEAD
$dirty = if (git -C $repo status --porcelain) { '-dirty' } else { '' }
$out = Join-Path $repo 'target\installer'
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force $out | Out-Null
$name = "Couch-Co-App_$($cargo)_$commit$dirty-setup.exe"
Copy-Item $built.FullName (Join-Path $out $name)
Write-Host "Built $name ($([math]::Round($built.Length / 1MB, 1)) MB) in $out."
Write-Host "Serve it with: npx --yes http-server target\installer -p 8000"
