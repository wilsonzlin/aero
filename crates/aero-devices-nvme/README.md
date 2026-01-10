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
- `MemoryBus` (guest physical memory DMA access)

## Interrupts

Only legacy INTx signalling is modelled (`NvmeController::intx_level`). MSI/MSI-X is not yet
implemented; this is sufficient for functional testing but may limit peak performance.

## Windows 7 notes

Windows 7 has no in-box NVMe driver. NVMe should be considered **experimental** for Win7
guests unless an NVMe driver is installed in the guest (e.g. Microsoft hotfixes KB2990941 +
KB3087873, or a vendor driver). This repository does not distribute third-party drivers.

