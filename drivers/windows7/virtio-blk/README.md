# aero_virtio_blk (virtio-blk StorPort miniport for Windows 7)

`aero_virtio_blk.sys` is a StorPort miniport driver intended for Windows 7 SP1 x86/x64.

> **AERO-W7-VIRTIO contract v1:** this driver binds to the virtio-blk **modern-only**
> PCI ID `PCI\VEN_1AF4&DEV_1042&REV_01` and validates `REV_01` at runtime.
>
> When using QEMU, pass:
> - `disable-legacy=on` (ensures the device enumerates as `DEV_1042`)
> - `x-pci-revision=0x01` (ensures the device enumerates as `REV_01`)

## Files

- `src/aero_virtio_blk.c` – StorPort miniport driver implementation.
- `include/aero_virtio_blk.h` – driver-local definitions.
- `inf/aero_virtio_blk.inf` – storage class INF for installation on Win7 x86/x64.

## Building

### Supported: WDK10 / MSBuild (CI path)

CI builds this driver via the MSBuild project:

- `drivers/windows7/virtio-blk/aero_virtio_blk.vcxproj`

From a Windows host with the WDK installed:

```powershell
# From the repo root:
.\ci\install-wdk.ps1
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json -Drivers windows7/virtio-blk
```

Build outputs are staged under:

- `out/drivers/windows7/virtio-blk/x86/aero_virtio_blk.sys`
- `out/drivers/windows7/virtio-blk/x64/aero_virtio_blk.sys`

To stage an installable/signable package, copy the built SYS into the package staging folder:

```text
drivers/windows7/virtio-blk/inf/aero_virtio_blk.sys
```

### Legacy/deprecated: WDK 7.1 `build.exe`

For local development you can also use the legacy WinDDK 7600 `build` utility (`sources`/`makefile` are kept for that workflow).

## Hardware IDs

The INF binds to the modern virtio-blk PCI ID:

- `PCI\VEN_1AF4&DEV_1042&REV_01` (modern-only virtio-blk; requires `disable-legacy=on` and `x-pci-revision=0x01`)

## Repo layout note (canonical driver)

This repository intentionally keeps **exactly one** `aero_virtio_blk` driver package that binds to
`PCI\VEN_1AF4&DEV_1042&REV_01` so CI builds and guest-tools packaging are deterministic.

The older duplicate under `drivers/win7/virtio-blk/` has been removed.

## Installation (non-boot disk)

1. Copy `inf/aero_virtio_blk.inf` and `aero_virtio_blk.sys` into the **same directory** on the guest.
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
