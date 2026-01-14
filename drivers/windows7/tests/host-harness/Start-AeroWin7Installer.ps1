# SPDX-License-Identifier: MIT OR Apache-2.0

[CmdletBinding()]
param(
  # QEMU system binary (e.g. qemu-system-x86_64 or qemu-system-i386)
  [Parameter(Mandatory = $true)]
  [string]$QemuSystem,

  # Path to the Windows 7 ISO (user-supplied; not redistributed).
  [Parameter(Mandatory = $true)]
  [string]$Win7IsoPath,

  # Output disk image for the Windows 7 installation.
  [Parameter(Mandatory = $true)]
  [string]$DiskImagePath,

  # If DiskImagePath does not exist, create a new qcow2 using qemu-img.
  [Parameter(Mandatory = $false)]
  [switch]$CreateDisk,

  # qemu-img binary (only needed when -CreateDisk is used).
  [Parameter(Mandatory = $false)]
  [string]$QemuImg = "qemu-img",

  [Parameter(Mandatory = $false)]
  [int]$DiskSizeGB = 32,

  # Optional provisioning ISO created by New-AeroWin7TestImage.ps1.
  # This is strongly recommended so Windows Setup can load virtio storage drivers (Load driver) and
  # you can run AERO\provision\provision.cmd after install.
  [Parameter(Mandatory = $false)]
  [string]$ProvisioningIsoPath = "",

  [Parameter(Mandatory = $false)]
  [int]$MemoryMB = 2048,

  [Parameter(Mandatory = $false)]
  [int]$Smp = 2,

  # If set, use QEMU's transitional virtio-pci devices (legacy + modern).
  # By default this script uses the Aero contract v1 virtio-pci identity:
  # - modern-only enumeration (`disable-legacy=on` => `DEV_1041`/`DEV_1042`)
  # - contract Revision ID (`x-pci-revision=0x01` => `REV_01`)
  # so strict Win7 driver packages can bind under QEMU.
  [Parameter(Mandatory = $false)]
  [switch]$VirtioTransitional,

  # Extra args passed verbatim to QEMU (advanced use: accel, machine type, display, etc.)
  [Parameter(Mandatory = $false)]
  [string[]]$QemuExtraArgs = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "AeroVirtioWin7QemuArgs.ps1")

if ($QemuSystem -match "[\\/\\\\]" -and (Test-Path -LiteralPath $QemuSystem -PathType Container)) {
  throw "-QemuSystem must be a QEMU system binary path (got a directory): $QemuSystem"
}
if ($QemuSystem -match "[\\/\\\\]") {
  if (-not (Test-Path -LiteralPath $QemuSystem -PathType Leaf)) {
    throw "-QemuSystem must be a QEMU system binary path (file not found): $QemuSystem"
  }
} else {
  try {
    $null = Get-Command -Name $QemuSystem -CommandType Application -ErrorAction Stop
  } catch {
    throw "-QemuSystem must be on PATH (qemu-system binary not found): $QemuSystem"
  }
}

if ($CreateDisk) {
  if ($QemuImg -match "[\\/\\\\]" -and (Test-Path -LiteralPath $QemuImg -PathType Container)) {
    throw "-QemuImg must be a qemu-img binary path (got a directory): $QemuImg"
  }
  if ($QemuImg -match "[\\/\\\\]") {
    if (-not (Test-Path -LiteralPath $QemuImg -PathType Leaf)) {
      throw "-QemuImg must be a qemu-img binary path (file not found): $QemuImg"
    }
  } else {
    try {
      $null = Get-Command -Name $QemuImg -CommandType Application -ErrorAction Stop
    } catch {
      throw "-QemuImg must be on PATH (qemu-img binary not found): $QemuImg"
    }
  }
}

if ($DiskSizeGB -le 0) {
  throw "-DiskSizeGB must be a positive integer."
}
if ($MemoryMB -le 0) {
  throw "-MemoryMB must be a positive integer."
}
if ($Smp -le 0) {
  throw "-Smp must be a positive integer."
}

$Win7IsoPath = (Resolve-Path -LiteralPath $Win7IsoPath).Path
if (Test-Path -LiteralPath $Win7IsoPath -PathType Container) {
  throw "-Win7IsoPath must be a file path (got a directory): $Win7IsoPath"
}

if (Test-Path -LiteralPath $DiskImagePath -PathType Container) {
  throw "-DiskImagePath must be a disk image file path (got a directory): $DiskImagePath"
}

if (-not (Test-Path -LiteralPath $DiskImagePath)) {
  if (-not $CreateDisk) {
    throw "DiskImagePath does not exist. Re-run with -CreateDisk (or provide an existing image): $DiskImagePath"
  }

  $diskParent = Split-Path -Parent $DiskImagePath
  if ([string]::IsNullOrEmpty($diskParent)) { $diskParent = "." }
  if (-not (Test-Path -LiteralPath $diskParent)) {
    New-Item -ItemType Directory -Path $diskParent -Force | Out-Null
  }

  Write-Host "Creating qcow2 disk: $DiskImagePath ($DiskSizeGB GB)"
  & $QemuImg create -f qcow2 $DiskImagePath "$($DiskSizeGB)G" | Out-Null
}

if (-not [string]::IsNullOrEmpty($ProvisioningIsoPath)) {
  $ProvisioningIsoPath = (Resolve-Path -LiteralPath $ProvisioningIsoPath).Path
  if (Test-Path -LiteralPath $ProvisioningIsoPath -PathType Container) {
    throw "-ProvisioningIsoPath must be a file path (got a directory): $ProvisioningIsoPath"
  }
}

Write-Host ""
Write-Host "Windows 7 install/provision flow"
Write-Host "--------------------------------"
Write-Host "1) Start Windows Setup; when asked where to install, click 'Load driver' if the virtio disk is not visible."
Write-Host "2) Browse the provisioning ISO and select the virtio storage driver INF under AERO\\drivers\\... (x86 vs x64)."
Write-Host "3) Complete Windows installation."
Write-Host "4) After first boot, run: <CD>:\\AERO\\provision\\provision.cmd (as Administrator) to install drivers + selftest + scheduled task."
Write-Host "5) Reboot. Then run Invoke-AeroVirtioWin7Tests.ps1 on the host to get deterministic PASS/FAIL via COM1 serial."
Write-Host ""

$osIsoDrive = "file=$(Quote-AeroWin7QemuKeyvalValue $Win7IsoPath),media=cdrom,readonly=on"

if ($VirtioTransitional) {
  $diskDrive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=virtio,cache=writeback"
  $netDevice = "virtio-net-pci,netdev=net0"

  $qemuArgs = @(
    "-m", "$MemoryMB",
    "-smp", "$Smp",
    "-boot", "d",
    "-drive", $diskDrive,
    "-drive", $osIsoDrive,
    "-netdev", "user,id=net0",
    "-device", $netDevice
  )
} else {
  # Ensure the QEMU binary supports the modern-only + contract revision properties we rely on.
  Assert-AeroWin7QemuSupportsAeroW7VirtioContractV1 -QemuSystem $QemuSystem

  # Force modern-only virtio-pci IDs (DEV_1041/DEV_1042) per AERO-W7-VIRTIO v1.
  # The shared QEMU arg helpers also set PCI Revision ID = 0x01 so strict contract-v1
  # drivers bind under QEMU.
  $diskDriveId = "drive0"
  $diskDrive = New-AeroWin7VirtioBlkDriveArg -DiskImagePath $DiskImagePath -DriveId $diskDriveId
  $diskDevice = New-AeroWin7VirtioBlkDeviceArg -DriveId $diskDriveId

  $qemuArgs = @(
    "-m", "$MemoryMB",
    "-smp", "$Smp",
    "-boot", "d",
    "-drive", $diskDrive,
    "-device", $diskDevice,
    "-drive", $osIsoDrive,
    "-netdev", "user,id=net0",
    "-device", (New-AeroWin7VirtioNetDeviceArg -NetdevId "net0")
  )
}

if (-not [string]::IsNullOrEmpty($ProvisioningIsoPath)) {
  $provIsoDrive = "file=$(Quote-AeroWin7QemuKeyvalValue $ProvisioningIsoPath),media=cdrom,readonly=on"
  $qemuArgs += @("-drive", $provIsoDrive)
} else {
  Write-Warning "No -ProvisioningIsoPath provided. You can still install Win7, but provisioning the drivers/selftest will be harder."
}

$qemuArgs += $QemuExtraArgs

Write-Host "Launching QEMU:"
Write-Host "  $QemuSystem $($qemuArgs -join ' ')"

& $QemuSystem @qemuArgs
