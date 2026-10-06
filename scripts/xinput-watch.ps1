# What an XInput game sees: each of the four XInput slots, live, as buttons and stick values.
#   .\scripts\xinput-watch.ps1            # 60 s
#   .\scripts\xinput-watch.ps1 -Secs 120
# Use it to tell our side from a game's: if the host's virtual pad shows every button and stick
# here while a guest plays, the stream is fine and the game is choosing another controller (on
# one PC, often the guest's own physical pad, which the game can see too). XInput needs no
# focus, so leave the stream window or the game in front, as you like. Ctrl+C stops it.

param([int]$Secs = 60)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class XInputWatch {
    [StructLayout(LayoutKind.Sequential)]
    public struct State { public uint Packet; public ushort Buttons; public byte LT; public byte RT; public short LX; public short LY; public short RX; public short RY; }
    [DllImport("xinput1_4.dll")] public static extern uint XInputGetState(uint slot, out State state);
}
'@

# Pairs, not a hashtable: indexing an ordered hashtable with a number picks the nth entry.
$names = @(
    @(0x0001, 'Up'), @(0x0002, 'Down'), @(0x0004, 'Left'), @(0x0008, 'Right'),
    @(0x0010, 'Start'), @(0x0020, 'Back'), @(0x0040, 'LS'), @(0x0080, 'RS'),
    @(0x0100, 'LB'), @(0x0200, 'RB'), @(0x0400, 'Guide'),
    @(0x1000, 'A'), @(0x2000, 'B'), @(0x4000, 'X'), @(0x8000, 'Y')
)
function Describe($s) {
    $buttons = [int]$s.Buttons
    $pressed = @($names | Where-Object { $buttons -band $_[0] } | ForEach-Object { $_[1] })
    # As int: the absolute value of -32768 doesn't fit a 16-bit number.
    $stick = { param([int]$x, [int]$y) if ([math]::Abs($x) -lt 8000 -and [math]::Abs($y) -lt 8000) { '-' } else { "$x,$y" } }
    "buttons [{0}]  LT {1} RT {2}  left {3}  right {4}" -f ($pressed -join ' '), $s.LT, $s.RT, (& $stick $s.LX $s.LY), (& $stick $s.RX $s.RY)
}

"Watching XInput slots 0 to 3 for $Secs s; a line appears whenever a slot changes."
$last = @{}
$deadline = (Get-Date).AddSeconds($Secs)
while ((Get-Date) -lt $deadline) {
    for ($slot = 0; $slot -lt 4; $slot++) {
        $s = New-Object XInputWatch+State
        $line = if ([XInputWatch]::XInputGetState($slot, [ref]$s) -eq 0) { Describe $s } else { 'not connected' }
        if ($last[$slot] -ne $line) {
            $last[$slot] = $line
            "{0:HH:mm:ss.f}  slot {1}: {2}" -f (Get-Date), $slot, $line
        }
    }
    Start-Sleep -Milliseconds 50
}
