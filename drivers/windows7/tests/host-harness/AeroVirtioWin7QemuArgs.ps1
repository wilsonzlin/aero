# SPDX-License-Identifier: MIT OR Apache-2.0

# Shared QEMU argument helpers for the Windows 7 virtio host harness.
#
# The Windows 7 virtio driver contract (AERO-W7-VIRTIO v1) requires *modern-only*
# virtio-pci devices. QEMU's `virtio-*-pci` devices are transitional by default and
# can expose the legacy I/O-port transport (PCI Device IDs 0x1000/0x1001).
#
# To force modern-only enumeration (PCI Device IDs 0x1041/0x1042) and prevent
# legacy transport from being exposed, pass `disable-legacy=on` on each virtio-pci
# device.

function New-AeroWin7VirtioNetDeviceArg {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $false)]
    [string]$NetdevId = "net0"
  )

  return "virtio-net-pci,netdev=$NetdevId,disable-legacy=on"
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

  return "virtio-blk-pci,drive=$DriveId,disable-legacy=on"
}

