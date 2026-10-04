# Start the desktop app once per profile, to test rooms on one machine:
#   .\scripts\profiles.ps1               # alice, bob and carol
#   .\scripts\profiles.ps1 dave erin     # other names
# First run `npm run tauri dev` in apps\desktop: it builds the app, serves the UI these
# instances load, and runs the default profile. Each profile has its own identity, rooms and
# webview data under %LOCALAPPDATA%\CouchCoApp\profiles\<name>.
# Debug builds are console programs. Each instance gets a console of its own with no window,
# so nothing pops up and closing this terminal (or Ctrl+C in it) doesn't touch them; their logs
# go to desktop.log and desktop.err.log in their profile folder. Close their windows when done.
# Invite links clicked in a browser open in the default profile; paste them into the others.

param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Names = @('alice', 'bob', 'carol')
)
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\debug\couch-desktop.exe'
if (-not (Test-Path $exe)) {
    throw "No debug build yet. Run 'npm run tauri dev' in apps\desktop first."
}
try {
    Invoke-WebRequest 'http://localhost:1420' -UseBasicParsing -TimeoutSec 3 | Out-Null
} catch {
    throw "The UI dev server isn't running. Run 'npm run tauri dev' in apps\desktop first."
}

foreach ($name in $Names) {
    # Lowercase only: the app lowercases profile names, so Alice and alice are one profile.
    if ($name -cnotmatch '^[a-z0-9_-]{1,32}$') {
        throw "Profile names are 1 to 32 lowercase letters, digits, '-' or '_': $name"
    }
}
foreach ($name in $Names) {
    $data = Join-Path $env:LOCALAPPDATA "CouchCoApp\profiles\$name"
    New-Item -ItemType Directory -Force $data | Out-Null
    $log = Join-Path $data 'desktop.log'
    $err = Join-Path $data 'desktop.err.log'
    # cmd does the redirection, inside the new windowless console, so the app needs no pipe
    # back to this script.
    $start = New-Object System.Diagnostics.ProcessStartInfo 'cmd.exe'
    $start.Arguments = "/d /c `"`"$exe`" > `"$log`" 2> `"$err`"`""
    $start.WorkingDirectory = $repo
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.EnvironmentVariables['COUCH_PROFILE'] = $name
    [System.Diagnostics.Process]::Start($start) | Out-Null
    Write-Host "Started $name; log: $log"
}
