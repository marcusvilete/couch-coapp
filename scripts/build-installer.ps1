# Builds the Windows installer (M10a) for another PC:
#   .\scripts\build-installer.ps1
#   npx --yes http-server target\installer -p 8000      # then, on the other PC, http://<this PC>:8000/
# It asks to install for "just me" (%LOCALAPPDATA%\Programs) or everyone (Program Files; on an
# administrator account Windows asks for administrator rights when it opens), registers couchco:// links,
# and after installing offers ViGEmBus and HidHide (their own setups, which ask for
# administrator rights). Unsigned (installers go to friends only, ADR 0009): Windows SmartScreen
# warns, and "More info" then "Run anyway" installs it. Logs go to desktop.log in
# %LOCALAPPDATA%\CouchCoApp\profiles\<profile>.
#
# The licences (M10b): the installer carries LICENSE and THIRD-PARTY-NOTICES.txt (`cargo xtask
# notices`), whose header links the app's source at this commit; FFmpeg's source and build scripts
# go beside the setup file, with SOURCES.txt. A release installer needs a clean tree, so the linked
# commit matches it exactly.
#
# Which relay it uses (M7) comes from the private release.toml, outside the repository
# (`cargo xtask release-config` creates it; deploy\release.example.toml explains it):
#   .\scripts\build-installer.ps1                       # release: our relay and its token, https only
#   .\scripts\build-installer.ps1 -Config <file>        # another file, e.g. a LAN test one (test = true)
#   .\scripts\build-installer.ps1 -TestRelay            # a test installer: iroh's public relays, no file needed

param(
    [string] $Config,
    [switch] $TestRelay
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'dev.ps1')
. (Join-Path $PSScriptRoot 'read-release-config.ps1')

# The values baked into the build (couch_engine::build_config). Checked here, since the app
# treats a value it can't read as none.
$build = @{ COUCH_BUILD_RELAY_URL = $null; COUCH_BUILD_RELAY_TOKEN = $null; COUCH_BUILD_INVITE_PAGE = $null; COUCH_BUILD_TEST = $null; COUCH_BUILD_COMMIT = $null }
$test = [bool] $TestRelay
$dirtyTree = [bool] (git -C $repo status --porcelain)
$build.COUCH_BUILD_COMMIT = (git -C $repo rev-parse --short=10 HEAD) + $(if ($dirtyTree) { '-dirty' } else { '' })
if ($TestRelay) {
    if ($Config) { throw 'Pass -Config or -TestRelay, not both.' }
    Write-Host "A test installer: iroh's public relays, and plain http relays on the LAN allowed."
} else {
    $path = if ($Config) { $Config } else { Get-ReleaseConfigPath }
    if (-not (Test-Path -LiteralPath $path)) {
        throw "No release configuration at $path. Run 'cargo xtask release-config', or build a test installer with -TestRelay."
    }
    Assert-OutsideRepo $path $repo
    $release = Read-ReleaseConfig $path
    $test = $release.test
    # A URL the app can't read would quietly make a build without our relay: parse it here.
    function Assert-WebAddress([string] $Name, [string] $Value) {
        $uri = $null
        $schemes = if ($test) { @('https', 'http') } else { @('https') }
        if (-not [Uri]::TryCreate($Value, [UriKind]::Absolute, [ref] $uri) -or
            $schemes -notcontains $uri.Scheme -or -not $uri.Host -or $uri.UserInfo) {
            throw "$Name must be an https:// address with a host$(if ($test) { ' (or http:// in a test file)' }), not '$Value'."
        }
    }
    if (-not $release.relay_url) {
        throw "$path has no relay_url yet. Until our relay is deployed, build test installers with -TestRelay."
    }
    Assert-WebAddress 'relay_url' $release.relay_url
    if (-not $release.relay_token) { throw "$path has no relay_token." }
    if ($release.invite_page) { Assert-WebAddress 'invite_page' $release.invite_page }
    $build.COUCH_BUILD_RELAY_URL = $release.relay_url
    $build.COUCH_BUILD_RELAY_TOKEN = $release.relay_token
    if ($release.invite_page) { $build.COUCH_BUILD_INVITE_PAGE = $release.invite_page }
    Write-Host "Our relay: $($release.relay_url)$(if ($test) { ' (a test build)' })"
}
if ($test) { $build.COUCH_BUILD_TEST = '1' }

# The notices link the app's source at this commit: it must be that exact commit, and reachable.
if ($dirtyTree) {
    if (-not $test) {
        throw 'Uncommitted changes: a release installer must match a commit exactly, since its notices link to that commit''s source. Commit first, or build a test installer (-TestRelay).'
    }
    Write-Warning 'Uncommitted changes: this test installer''s notices ask friends to request its source from you.'
}
if (-not (git -C $repo branch -r --contains HEAD 2>$null)) {
    Write-Warning 'This commit is on no remote branch yet: push it, or friends can''t reach the source the notices link to.'
}

# The pinned DLLs, driver installers and FFmpeg's sources; the notices; then all of it next to
# the release build, where the bundle takes it.
cargo xtask fetch-deps
if ($LASTEXITCODE -ne 0) { throw 'fetch-deps failed' }
cargo xtask notices
if ($LASTEXITCODE -ne 0) { throw 'notices failed (is cargo-about installed? cargo xtask doctor)' }
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
    # Set for this build only (and unset ones a terminal may carry), so a later
    # `npm run tauri dev` here doesn't get them.
    foreach ($name in $build.Keys) {
        if ($null -eq $build[$name]) {
            Remove-Item "Env:$name" -ErrorAction SilentlyContinue
        } else {
            Set-Item "Env:$name" $build[$name]
        }
    }
    # The installer's own settings (the bundled files, the driver offers) live apart from
    # tauri.conf.json: Tauri's build step copies bundled files next to the app it builds,
    # which in development would overwrite the debug stream window with the release one.
    # Its beforeBuildCommand builds couch-viewer (release) and the front end first.
    npx tauri build --config src-tauri/tauri.installer.conf.json
    if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
} finally {
    foreach ($name in $build.Keys) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
    Pop-Location
}

$built = Get-ChildItem (Join-Path $repo 'target\release\bundle\nsis') -Filter "*_$($cargo)_x64-setup.exe" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $built) { throw 'No installer was produced.' }

$commit = git -C $repo rev-parse --short HEAD
$dirty = if ($dirtyTree) { '-dirty' } else { '' }
$out = Join-Path $repo 'target\installer'
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force $out | Out-Null
$kind = if ($test) { '-test' } else { '' }
$name = "Couch-Co-App_$($cargo)_$commit$dirty$kind-setup.exe"
Copy-Item $built.FullName (Join-Path $out $name)

# FFmpeg's source and build scripts go with every installer (the LGPL), explained in SOURCES.txt.
$sources = Join-Path $repo 'third_party\sources'
Get-ChildItem $sources -Filter *.tar.gz | Copy-Item -Destination $out
$repository = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^repository = "([^"]+)"').Matches | Select-Object -First 1
$appSource = if ($repository -and -not $dirtyTree) {
    "$($repository.Groups[1].Value)/tree/$(git -C $repo rev-parse HEAD)"
} else {
    'ask whoever gave you this installer: they must provide it.'
}
$readme = @"
Couch Co-App $cargo, the files beside its installer

$name
  The installer. Couch Co-App is free software under the GNU GPL, version 3
  (the LICENSE file it installs). Its third-party licences are in
  THIRD-PARTY-NOTICES.txt, installed next to the app and shown in
  Settings -> About.

The app's source code, for exactly this installer:
  $appSource

FFmpeg, which the app includes as DLLs, is under the LGPL. Its source, and
the scripts that built the included copy, are these two archives:


"@
foreach ($file in Get-ChildItem $out -Filter *.tar.gz) {
    $readme += "$($file.Name)`r`n  SHA-256 $((Get-FileHash $file.FullName -Algorithm SHA256).Hash.ToLower())`r`n`r`n"
}
Set-Content -LiteralPath (Join-Path $out 'SOURCES.txt') -Value $readme -Encoding ASCII

Write-Host "Built $name ($([math]::Round($built.Length / 1MB, 1)) MB) in $out, with FFmpeg's sources and SOURCES.txt."
Write-Host "Serve it with: npx --yes http-server target\installer -p 8000"
