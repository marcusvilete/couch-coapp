# Several viewers of one host on this PC: what a host's encoders carry, and whether rate
# control keeps each viewer's stream up (docs/m5a-results.md, "Viewers on this PC").
#   .\scripts\viewers.ps1                 # 4 viewers, 20 s
#   .\scripts\viewers.ps1 -Viewers 7      # the most a session takes
#   .\scripts\viewers.ps1 -Screen         # capture the primary screen instead (M9b: GPU frames)
#   .\scripts\viewers.ps1 -Screen -CopyThroughMemory   # the same, frames through memory as before M9b
#   .\scripts\viewers.ps1 -Screen -CopyFrames          # the viewers copy decoded frames through memory, as before M9c
# Streams a 1080p60 test pattern with `couch-cli host` and starts that many `couch-cli view`
# windows (small, in a grid), then prints each viewer's fps, capture-to-present latency and
# received bitrate, the host's last per-viewer line (bitrate, target, slowest encode), and the
# stream windows' CPU use (all of them together, measured over the middle of the run).
# The test pattern is made in memory, so only -Screen measures the capture's GPU path; the
# viewer windows on that screen keep it changing every frame.
# On one PC the GPU also decodes every stream, so this overstates a real host's load.
# Needs the release build: cargo build --release -p couch-cli -p couch-viewer
# Logs go to %TEMP%\couch-viewers-<N>. Leave the machine alone while it runs.

param([int]$Viewers = 4, [int]$Secs = 20, [switch]$Screen, [switch]$CopyThroughMemory, [switch]$CopyFrames)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path .\target\release\couch-cli.exe)) {
    throw 'No release build: run cargo build --release -p couch-cli -p couch-viewer from the repo root.'
}
$cli = Resolve-Path .\target\release\couch-cli.exe
$dir = Join-Path $env:TEMP "couch-viewers-$Viewers"
New-Item -ItemType Directory -Force $dir | Out-Null
Get-ChildItem $dir | Remove-Item -Force

function Start-Hidden($argline, $out) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = 'cmd.exe'
    $psi.Arguments = "/d /c `"`"$cli`" $argline > `"$out`" 2>nul`""
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    [System.Diagnostics.Process]::Start($psi)
}

$hostOut = Join-Path $dir 'host.txt'
$source = if ($Screen) { '--source monitor' } else { '--source testpattern --pattern-size 1920 1080' }
# The host inherits this process's environment. A value already set (to try
# something) is kept, and restored afterwards.
$hostSwitch = $env:COUCH_HOST_ZERO_COPY
if ($CopyThroughMemory) { $env:COUCH_HOST_ZERO_COPY = '0' }
$h = Start-Hidden "host $source --fps 60 --relay none --max-viewers 7 --audio off" $hostOut
$env:COUCH_HOST_ZERO_COPY = $hostSwitch
$ticket = $null
for ($i = 0; $i -lt 100 -and -not $ticket; $i++) {
    Start-Sleep -Milliseconds 200
    if (Test-Path $hostOut) {
        $line = Get-Content $hostOut -ErrorAction SilentlyContinue | Where-Object { $_ -like 'TICKET *' } | Select-Object -First 1
        if ($line) { $ticket = $line.Substring(7) }
    }
}
if (-not $ticket) { throw 'no ticket' }

$views = @()
# The viewers (and their stream windows) inherit this process's environment,
# so COUCH_VIEWER_ZERO_COPY=1 set beforehand tries zero-copy on another GPU.
$viewerSwitch = $env:COUCH_VIEWER_ZERO_COPY
if ($CopyFrames) { $env:COUCH_VIEWER_ZERO_COPY = '0' }
for ($v = 0; $v -lt $Viewers; $v++) {
    $x = 20 + ($v % 4) * 470
    $y = 40 + [math]::Floor($v / 4) * 300
    $out = Join-Path $dir "view$v.txt"
    $views += Start-Hidden "view $ticket --relay none --measure-secs $Secs --no-hud --viewer-size 460 260 --viewer-pos $x $y" $out
    Start-Sleep -Milliseconds 300
}
$env:COUCH_VIEWER_ZERO_COPY = $viewerSwitch

# The stream windows' CPU time over the middle of the run, after a warm-up.
# It counts every couch-viewer on the PC (close the app first), and needs
# -Secs 15 or more so the windows are still open for the second sample.
function Get-ViewerCpu { $t = @{}; Get-Process couch-viewer -ErrorAction SilentlyContinue | ForEach-Object { $t[$_.Id] = $_.TotalProcessorTime.TotalSeconds }; $t }
$window = [math]::Max(5, $Secs - 10)
Start-Sleep -Seconds 5
$before = Get-ViewerCpu
Start-Sleep -Seconds $window
$after = Get-ViewerCpu
$cpu = 0.0
foreach ($id in $after.Keys) { if ($before.ContainsKey($id)) { $cpu += $after[$id] - $before[$id] } }

foreach ($p in $views) { $p.WaitForExit(($Secs + 30) * 1000) | Out-Null }
Start-Sleep -Seconds 1
Get-CimInstance Win32_Process -Filter "ParentProcessId=$($h.Id)" | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
Stop-Process -Id $h.Id -Force -ErrorAction SilentlyContinue
Get-Process couch-cli -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

for ($v = 0; $v -lt $Viewers; $v++) {
    $r = Get-Content (Join-Path $dir "view$v.txt") | Where-Object { $_ -like 'RESULT *' } | Select-Object -First 1
    if (-not $r) { "viewer $($v+1): no result"; continue }
    $j = $r.Substring(7) | ConvertFrom-Json
    "viewer {0}: fps {1:N1}  capture->present p50 {2:N1} p95 {3:N1} ms  received p50 {4:N1} Mbit/s  decode errors {5}  ended early {6}" -f ($v+1), $j.fps, $j.capture_to_present_ms.p50, $j.capture_to_present_ms.p95, $j.receive_mbps.p50, $j.decode_errors, $j.ended_early
}
"stream windows' CPU: {0:N0}% of one core in total ({1} windows, over {2} s)" -f ($cpu / $window * 100), $after.Count, $window
"host, last lines:"
Get-Content $hostOut | Where-Object { $_ -match 'fps' } | Select-Object -Last $Viewers
