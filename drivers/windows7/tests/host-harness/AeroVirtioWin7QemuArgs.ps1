# SPDX-License-Identifier: MIT OR Apache-2.0

# Shared QEMU argument helpers for the Windows 7 virtio host harness.
#
# The Windows 7 virtio driver contract (AERO-W7-VIRTIO v1) requires *modern-only*
# virtio-pci devices. QEMU's `virtio-*-pci` devices are transitional by default and
# can expose the legacy I/O-port transport (PCI Device IDs 0x10xx, e.g. 0x1000/0x1001/0x1011).
#
# To force modern-only enumeration (e.g. virtio-net `DEV_1041`, virtio-blk `DEV_1042`,
# virtio-input `DEV_1052`) and prevent legacy transport from being exposed, pass
# `disable-legacy=on` on each virtio-pci device.
#
# The contract major version is encoded in the PCI Revision ID. Contract v1 requires
# Revision ID 0x01, but some QEMU virtio devices report REV_00 by default. Force
# `x-pci-revision=0x01` so strict contract drivers bind under QEMU.

function Get-AeroWin7QemuDeviceHelpText {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$QemuSystem,
    [Parameter(Mandatory = $true)]
    [string]$DeviceName
  )

  $help = & $QemuSystem -device "$DeviceName,help" 2>&1
  $exitCode = $LASTEXITCODE
  if ($exitCode -ne 0) {
    $helpText = ($help | Out-String).Trim()
    throw "Failed to query QEMU device help for '$DeviceName' ($QemuSystem). Output:`n$helpText"
  }

  return ($help | Out-String)
}

function Assert-AeroWin7QemuSupportsAeroW7VirtioContractV1 {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$QemuSystem,

    # If set, also validate virtio-input device availability/properties (keyboard + mouse).
    [Parameter(Mandatory = $false)]
    [switch]$WithVirtioInput
  )

  $devices = @(
    @{ Name = "virtio-net-pci"; RequireDisableLegacy = $true },
    @{ Name = "virtio-blk-pci"; RequireDisableLegacy = $true }
  )

  if ($WithVirtioInput) {
    $devices += @(
      @{ Name = "virtio-keyboard-pci"; RequireDisableLegacy = $true },
      @{ Name = "virtio-mouse-pci"; RequireDisableLegacy = $true }
    )
  }

  foreach ($d in $devices) {
    $name = $d.Name
    $helpText = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName $name

    if ($d.RequireDisableLegacy -and ($helpText -notmatch "(?m)^\s*disable-legacy\b")) {
      throw "QEMU device '$name' does not expose 'disable-legacy'. AERO-W7-VIRTIO v1 requires modern-only virtio-pci enumeration. Upgrade QEMU."
    }

    if ($helpText -notmatch "(?m)^\s*x-pci-revision\b") {
      throw "QEMU device '$name' does not expose 'x-pci-revision'. AERO-W7-VIRTIO v1 requires PCI Revision ID 0x01. Upgrade QEMU."
    }
  }
}

function New-AeroWin7VirtioNetDeviceArg {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $false)]
    [string]$NetdevId = "net0"
  )

  return "virtio-net-pci,netdev=$NetdevId,disable-legacy=on,x-pci-revision=0x01"
}

function New-AeroWin7VirtioBlkDriveArg {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$DiskImagePath,

    [Parameter(Mandatory = $false)]
    [string]$DriveId = "drive0",

    # If set, discard disk writes on exit (QEMU drive snapshot=on).
    [Parameter(Mandatory = $false)]
    [switch]$Snapshot
  )

  $drive = "file=$DiskImagePath,if=none,id=$DriveId,cache=writeback"
  if ($Snapshot) {
    $drive += ",snapshot=on"
  }
  return $drive
}

function New-AeroWin7VirtioBlkDeviceArg {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $false)]
    [string]$DriveId = "drive0"
  )

  return "virtio-blk-pci,drive=$DriveId,disable-legacy=on,x-pci-revision=0x01"
}

function New-AeroWin7VirtioKeyboardDeviceArg {
  [CmdletBinding()]
  param()

  return "virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01"
}

function New-AeroWin7VirtioMouseDeviceArg {
  [CmdletBinding()]
  param()

  return "virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01"
}
