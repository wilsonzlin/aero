# `aero-devices-nvme`

Minimal NVMe PCI storage controller emulation for Aero.

## Scope

This crate implements an NVMe controller with:

- BAR0 registers (`CAP/VS/CC/CSTS/AQA/ASQ/ACQ` + doorbells)
- Admin queues (SQ/CQ, QID 0)
- I/O queues created via admin commands
- Commands:
  - Admin: `IDENTIFY`, `CREATE IO CQ`, `CREATE IO SQ`
  - I/O: `READ`, `WRITE`, `FLUSH`
- DMA:
  - PRP1/PRP2 + PRP list support
  - SGL is **not** supported (returns `INVALID_FIELD`)

The controller is intentionally implemented against two small traits:

- `DiskBackend` (block storage backend)
- `memory::MemoryBus` (guest physical memory DMA access)

Note: the repo-wide canonical synchronous disk trait is `aero_storage::VirtualDisk`; `DiskBackend`
is an internal integration trait for the NVMe model. See:

- [`docs/20-storage-trait-consolidation.md`](../../docs/20-storage-trait-consolidation.md)

## Using `aero-storage` disks as the backend

Many Aero disk image formats are implemented in the [`aero-storage`](../aero-storage/) crate
behind the [`aero_storage::VirtualDisk`] trait.

To use an `aero-storage` disk with the NVMe controller without duplicating disk abstractions,
wrap it with an adapter:

- [`aero_devices_nvme::AeroStorageDiskAdapter`]: wraps a `Box<dyn aero_storage::VirtualDisk + Send>`
  as an NVMe [`DiskBackend`]. This adapter performs explicit range/alignment checks so the NVMe
  controller can surface `DiskError::OutOfRange` / `DiskError::UnalignedBuffer`.
- [`aero_devices_nvme::NvmeDiskFromAeroStorage`]: a generic convenience wrapper for concrete
  `aero_storage` disk types, primarily useful outside trait objects.

For the common case where the disk is already behind a `Box<dyn aero_storage::VirtualDisk + Send>`,
you can use [`aero_devices_nvme::from_virtual_disk`] (returns `DiskResult`) or construct a
controller/device directly:

Alternatively, use [`NvmeController::try_new_from_aero_storage`] /
[`NvmePciDevice::try_new_from_aero_storage`] to construct a controller/device directly from an
`aero-storage` disk.

## Interrupts

Only legacy INTx signalling is modelled (`NvmeController::intx_level`). MSI/MSI-X is not yet
implemented; this is sufficient for functional testing but may limit peak performance.

## Windows 7 notes

Windows 7 has no in-box NVMe driver. NVMe should be considered **experimental** for Win7
guests unless an NVMe driver is installed in the guest (e.g. Microsoft hotfixes KB2990941 +
KB3087873, or a vendor driver). This repository does not distribute third-party drivers.
