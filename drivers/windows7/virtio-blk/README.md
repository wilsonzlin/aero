# aero_virtio_blk (virtio-blk StorPort miniport for Windows 7)

`aero_virtio_blk.sys` is a StorPort miniport driver intended for Windows 7 SP1 x86/x64.

> **AERO-W7-VIRTIO contract v1:** this driver binds to the virtio-blk **modern-only**
> PCI ID `PCI\VEN_1AF4&DEV_1042&REV_01` and validates `REV_01` at runtime.
>
> When using QEMU, pass:
> - `disable-legacy=on` (ensures the device enumerates as `DEV_1042`)
> - `x-pci-revision=0x01` (ensures the device enumerates as `REV_01`)

## Building

CI builds this driver with a modern WDK (currently pinned to 10.0.22621.0) via the MSBuild project `aero_virtio_blk.vcxproj`.

For local development you can use either:

- `aero_virtio_blk.vcxproj` (Visual Studio / MSBuild + WDK 10), or
- the legacy WinDDK 7600 `build` utility (`sources`/`makefile` are kept for that workflow).

## Hardware IDs

The INF binds to the modern virtio-blk PCI ID:

- `PCI\VEN_1AF4&DEV_1042&REV_01` (modern-only virtio-blk; requires `disable-legacy=on` and `x-pci-revision=0x01`)

## Repo layout note (canonical driver)

This repository intentionally keeps **exactly one** `aero_virtio_blk` driver package that binds to
`PCI\VEN_1AF4&DEV_1042&REV_01` so CI builds and guest-tools packaging are deterministic.

The older duplicate under `drivers/win7/virtio-blk/` has been removed.

## Installation (non-boot disk)

1. Copy `aero_virtio_blk.sys` and `inf/aero_virtio_blk.inf` onto the guest.
2. In Device Manager, update the driver for the unknown storage controller and point it at the INF.
3. The disk should appear via `disk.sys` and be visible in Disk Management.

## Boot disk usage

The INF installs the service as `StartType = 0` and `LoadOrderGroup = "SCSI Miniport"` so it can be used as a boot-start storage driver when the system disk is exposed as virtio-blk.

For offline image integration, inject the driver into the Windows image and ensure the PCI hardware ID is present in the critical device database (integration handled by separate tooling/tasks).

## Diagnostics

The driver supports a minimal `IOCTL_SCSI_MINIPORT` query:

- `SRB_IO_CONTROL.Signature = "AEROVBLK"`
- `SRB_IO_CONTROL.ControlCode = 0x8000A001`

Returns `AEROVBLK_QUERY_INFO` (negotiated features + queue stats).
