# `aero-devices-nvme`

Minimal NVMe PCI storage controller emulation for Aero.

## Scope

This crate implements an NVMe controller with:

- BAR0 registers (`CAP/VS/CC/CSTS/AQA/ASQ/ACQ` + doorbells)
- Admin queues (SQ/CQ, QID 0)
- I/O queues created via admin commands
- Commands:
  - Admin: `IDENTIFY`, `CREATE IO CQ`, `CREATE IO SQ`
  - I/O: `READ`, `WRITE`, `FLUSH`, `WRITE ZEROES`, `DATASET MANAGEMENT (DSM deallocate)`
- DMA:
  - PRP1/PRP2 + PRP list support
  - Limited SGL support for data transfers (e.g. `IDENTIFY`, `READ`, `WRITE`, DSM range lists):
    - Data Block descriptors (address + length)
    - Segment / Last Segment chaining (bounded)

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

## Reverse adapter: using an NVMe backend as an `aero-storage` disk

In rare cases, you may already have an NVMe [`DiskBackend`] implementation (e.g. a platform-specific
backend or a test stub), but want to layer `aero-storage` disk wrappers (cache/sparse/overlay/etc)
on top of it.

Use [`aero_devices_nvme::NvmeBackendAsAeroVirtualDisk`] to adapt an NVMe backend into an
[`aero_storage::VirtualDisk`]. Note that NVMe backends are sector-addressed; this adapter only
supports *sector-aligned* byte offsets and lengths.

## Interrupts

- Legacy INTx signalling is modelled via `NvmeController::intx_level` and exposed via
  `NvmePciDevice::irq_level()`.
- The PCI wrapper (`NvmePciDevice`) supports message-signaled interrupts when the platform attaches
  an `aero_platform::interrupts::msi::MsiTrigger` sink via `NvmePciDevice::set_msi_target`:
  - **MSI**: single-vector (`MsiCapability`).
  - **MSI-X**: BAR0-backed table + PBA (single vector; table entry 0).
- When MSI-X is enabled it is used in preference to MSI; when neither is enabled (or no MSI sink is
  attached), the device falls back to legacy INTx.
- MSI-X table/PBA programming state is preserved across snapshot/restore.

## Best-effort semantics

Some storage maintenance commands depend on backend support that is not universally available
(especially in wasm32/browser storage backends).

- `WRITE ZEROES` is implemented by writing an actual zero-filled buffer to the backend (bounded by
  the controller's `NVME_MAX_DMA_BYTES` limit).
- `DSM deallocate` parses and validates the range list, then best-effort forwards discard/TRIM
  requests to the backend. Backends that cannot reclaim storage may treat discard as a no-op
  success.

## Windows 7 notes

Windows 7 has no in-box NVMe driver. NVMe should be considered **experimental** for Win7
guests unless an NVMe driver is installed in the guest (e.g. Microsoft hotfixes KB2990941 +
KB3087873, or a vendor driver). This repository does not distribute third-party drivers.
