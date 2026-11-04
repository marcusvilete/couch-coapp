# Makes the Linux test VM (the linux-host-vm-run skill): Ubuntu's desktop
# ISO attached, ready for its installer. The VM lives in VirtualBox's own
# folder (an SSD), never in the repository; the ISO may be anywhere.
#
#   .\scripts\vm\create.ps1 -Iso D:\VMs\iso\ubuntu-26.04.1-desktop-amd64.iso
#
# Wayland needs VMSVGA with 3D; PipeWire needs an audio device; bridged
# networking lets a Windows guest on this PC reach the VM directly. Sound:
# AC97 through DirectSound. On a PC running Hyper-V (WSL2), WASAPI made it
# choppy and Intel HD Audio made it noise (VirtualBox 7.2, 2026-11-03).
param(
    [Parameter(Mandatory)] [string] $Iso,
    [string] $Name = "couch-linux",
    [int] $Cpus = 4,
    [int] $MemoryMb = 8192,
    [int] $DiskGb = 40,
    # The host's adapter to bridge to: the first wired one that's up, by default.
    [string] $Adapter = ""
)
$ErrorActionPreference = "Stop"
$vbox = "C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
if (-not (Test-Path $vbox)) { throw "VirtualBox isn't installed ($vbox)" }
if (-not (Test-Path $Iso)) { throw "no ISO at $Iso" }

# VBoxManage writes its progress to stderr, which Windows PowerShell would
# take for an error under Stop: its exit code decides.
function VBox {
    $ErrorActionPreference = "Continue"
    & $vbox @args 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "VBoxManage $($args -join ' ') failed" }
}

if ((& $vbox list vms) -match "^`"$Name`"") { throw "a VM named $Name exists already" }
if (-not $Adapter) {
    $Adapter = (Get-NetAdapter | Where-Object { $_.Status -eq "Up" -and $_.InterfaceDescription -notmatch "Hyper-V|Virtual" } |
        Select-Object -First 1).InterfaceDescription
    if (-not $Adapter) { throw "no adapter to bridge to: name one with -Adapter" }
}
Write-Host "bridging to: $Adapter"

VBox createvm --name $Name --ostype Ubuntu_64 --register
$folder = Split-Path (((& $vbox showvminfo $Name --machinereadable) -match '^CfgFile=') -replace '^CfgFile="(.*)"$', '$1')
VBox modifyvm $Name --cpus $Cpus --memory $MemoryMb --firmware efi `
    --graphicscontroller vmsvga --vram 128 --accelerate-3d on `
    --audio-enabled on --audio-out on --audio-in off `
    --audio-controller ac97 --audio-codec stac9700 --audio-driver dsound `
    --nic1 bridged --bridge-adapter1 $Adapter `
    --usb-xhci on --clipboard-mode bidirectional --mouse usbtablet
$disk = Join-Path $folder "$Name.vdi"
VBox createmedium disk --filename $disk --size ($DiskGb * 1024) --format VDI
VBox storagectl $Name --name SATA --add sata --controller IntelAhci --portcount 2
# As an SSD with discard: what Ubuntu frees shrinks the disk file again.
VBox storageattach $Name --storagectl SATA --port 0 --device 0 --type hdd --medium $disk `
    --nonrotational on --discard on
VBox storageattach $Name --storagectl SATA --port 1 --device 0 --type dvddrive --medium $Iso
Write-Host "made $Name in $folder; start it with: & '$vbox' startvm $Name"
