# aerovblk (virtio-blk StorPort miniport for Windows 7)

`aerovblk.sys` is a StorPort miniport driver intended for Windows 7 SP1 x86/x64.

## Hardware IDs

The INF binds to the standard virtio-blk PCI ID used by QEMU/virtio:

- `PCI\VEN_1AF4&DEV_1001`
- `PCI\VEN_1AF4&DEV_1042` (virtio 1.0 transitional virtio-blk)

## Installation (non-boot disk)

1. Copy `aerovblk.sys` and `aerovblk.inf` onto the guest.
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
