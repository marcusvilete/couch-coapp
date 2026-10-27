<#
.SYNOPSIS
Screenshots of the stream window's HUD in every `--hud-preview` state (M21),
into target\ui-shots\viewer.

.DESCRIPTION
Starts couch-viewer once per state, waits for it to draw, captures its window
with PrintWindow (which sees GPU-drawn windows even when covered) and closes
it. Each state at 1280x720; the crowded one also at 480x300 and at 150%; a
pointer resting on a seat chip shows its hover and tooltip. Fails if a capture
comes back black. Build the viewer first: cargo build -p couch-viewer.

.EXAMPLE
.\scripts\viewer-shots.ps1
.\scripts\viewer-shots.ps1 -Only crowded,hover -Release
#>
param(
    # Shoot only these shots, by name.
    [string[]]$Only,
    # Use the release build.
    [switch]$Release,
    [string]$Out
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$exe = Join-Path $root "target\$(if ($Release) { 'release' } else { 'debug' })\couch-viewer.exe"
if (-not (Test-Path $exe)) { throw "No ${exe}: build it first (cargo build -p couch-viewer)." }
if (-not $Out) { $Out = Join-Path $root 'target\ui-shots\viewer' }
New-Item -ItemType Directory -Force $Out | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class ViewerShots {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[ViewerShots]::SetProcessDPIAware() | Out-Null

$states = 'watching', 'asking', 'controlling', 'asking-more', 'paused', 'stopped', 'waiting',
    'beside', 'single', 'muted', 'crowded', 'worst'
$shots = [ordered]@{}
foreach ($state in $states) { $shots[$state] = @('--hud-preview', $state, '--hud-scale', '1') }
$shots['crowded-480x300'] = @('--hud-preview', 'crowded', '--size', '480', '300', '--hud-scale', '1')
$shots['crowded-150'] = @('--hud-preview', 'crowded', '--hud-scale', '1.5')
# The pointer on Controller 2's chip, which can be asked for.
$shots['hover'] = @('--hud-preview', 'watching', '--hud-scale', '1', '--hud-preview-pointer', '620', '78')

$failed = @()
foreach ($name in $shots.Keys) {
    if ($Only -and $name -notin $Only) { continue }
    $file = Join-Path $Out "$name.png"
    $p = Start-Process $exe -ArgumentList $shots[$name] -PassThru -WindowStyle Normal `
        -RedirectStandardError (Join-Path $Out "$name.log")
    try {
        $deadline = (Get-Date).AddSeconds(15)
        while ((Get-Date) -lt $deadline) {
            $p.Refresh()
            if ($p.HasExited) { throw "$name`: the viewer exited ($($p.ExitCode)); see $name.log" }
            if ($p.MainWindowHandle -ne [IntPtr]::Zero) { break }
            Start-Sleep -Milliseconds 100
        }
        if ($p.MainWindowHandle -eq [IntPtr]::Zero) { throw "$name`: no window" }
        # Fonts, the first layout and a tooltip's delay.
        Start-Sleep -Milliseconds 1500
        $h = $p.MainWindowHandle
        $r = New-Object ViewerShots+RECT
        [ViewerShots]::GetWindowRect($h, [ref]$r) | Out-Null
        $bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $dc = $g.GetHdc()
        # 2 = PW_RENDERFULLCONTENT: GPU-drawn windows too.
        [ViewerShots]::PrintWindow($h, $dc, 2) | Out-Null
        $g.ReleaseHdc($dc)
        $g.Dispose()
        $lit = 0
        for ($x = 8; $x -lt $bmp.Width; $x += 37) {
            for ($y = 8; $y -lt $bmp.Height; $y += 29) {
                if ($bmp.GetPixel($x, $y).GetBrightness() -gt 0.08) { $lit++ }
            }
        }
        $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        if ($lit -eq 0) { $failed += $name; Write-Warning "$name`: black" }
        else { "$name.png" }
    } finally {
        if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
        $p.WaitForExit(5000) | Out-Null
    }
}
"Shots in $Out"
if ($failed) { throw "Black captures: $($failed -join ', ')" }
