# `aero-storage`

This crate contains Aero’s virtual disk abstractions and pure Rust disk image formats.

In the browser, the primary persistence backend is OPFS. Aero provides a Rust/wasm32
implementation in `crates/aero-opfs` (e.g. `aero_opfs::OpfsByteStorage`) that implements
`aero_storage::StorageBackend`/`aero_storage::VirtualDisk`.

On native (non-wasm32) targets, `aero-storage` also includes a first-party
`std::fs::File`-backed implementation: `aero_storage::StdFileBackend` (also available as the
`aero_storage::FileBackend` alias). This makes it easy to
write host-side tooling (inspection, conversion, regression tests) that works directly on disk
images.

```rust,no_run
use aero_storage::{DiskImage, FileBackend, VirtualDisk};

let backend = FileBackend::open_rw("disk.img").unwrap();
let mut disk = DiskImage::open_auto(backend).unwrap();

let mut sector = [0u8; 512];
disk.read_sectors(0, &mut sector).unwrap();
```

Higher-level orchestration such as remote HTTP streaming/caching and UI integration may
still live in the TypeScript host layer.

Note: IndexedDB-based storage is async and is not currently exposed as a synchronous
`aero_storage::StorageBackend` / `aero_storage::VirtualDisk` for the boot-critical controller path;
see:

- [`docs/19-indexeddb-storage-story.md`](../../docs/19-indexeddb-storage-story.md)
- [`docs/20-storage-trait-consolidation.md`](../../docs/20-storage-trait-consolidation.md)

The async IndexedDB block store lives in `crates/st-idb`.

## Supported disk image formats

- Raw (`RawDisk`): byte-for-byte disk image (no header).
- Aero Sparse (`AEROSPAR`, v1): Aero-specific sparse format (`AeroSparseDisk`).
- Aero COW overlay: copy-on-write overlay built on top of a base disk (`AeroCowDisk`).
- QCOW2 (`Qcow2Disk`):
  - v2 and v3 headers
  - `cluster_bits` in `9..=21`
  - no backing files, encryption, compression, or internal snapshots
  - v3 "zero cluster" flag is treated as unallocated (reads as zero)
- VHD (`VhdDisk`):
  - Fixed (type 2)
  - Dynamic (type 3): BAT + bitmap + block allocation

For convenience, `DiskImage::open_auto` can auto-detect and open these formats from a
single `StorageBackend`.

## Using `aero-storage` disks with device models

Device models such as NVMe and virtio-blk live in separate crates and may use their own disk/backend
traits. To avoid duplicating disk abstractions, the workspace provides adapters so an
[`aero_storage::VirtualDisk`] can be used consistently across controllers:

- `aero-devices-nvme` (`DiskBackend`)
- `aero-devices` device backends (`storage::DiskBackend`)
- `aero-virtio` virtio-blk (`devices::blk::BlockBackend`)
- legacy `emulator` storage models (`io::storage::disk::DiskBackend`)

Notes:

- The shared adapter wrapper *types* live in `crates/aero-storage-adapters/` and are typically
  re-exported as `AeroStorageDiskAdapter` by device crates.
- `aero-virtio` additionally supports wiring a boxed `VirtualDisk` directly as a virtio-blk backend
  (see `aero_virtio::devices::blk::VirtioBlkDisk`).
- If you need the reverse direction (wrapping an existing device/backend trait object as an
  `aero_storage::VirtualDisk` so you can layer `aero-storage` disk wrappers like caches/overlays on
  top), see the reverse adapters in the corresponding device crates (e.g.
  `aero_devices_nvme::NvmeBackendAsAeroVirtualDisk`,
  `aero_devices::storage::DeviceBackendAsAeroVirtualDisk`,
  `aero_virtio::devices::blk::BlockBackendAsAeroVirtualDisk`).

## Aero Sparse (`AEROSPAR`) format (v1)

The sparse format is optimized for representing *huge* virtual disks (20–40GB+) while
only consuming space for blocks that have actually been written.

Layout (little-endian):

- Header (64 bytes)
- Allocation table: `table_entries` × `u64`
  - Each entry stores the **physical byte offset** of the corresponding block data
  - `0` means “unallocated”
- Data blocks: fixed-size `block_size_bytes` chunks, appended sequentially

Typical configuration:

- `block_size_bytes = 1 MiB` (power-of-two, multiple of 512)
- `disk_size_bytes = 40 GiB`

The allocation table for a 40 GiB disk at 1 MiB blocks is ~320 KiB, small enough to
load eagerly.

## Copy-on-write overlay

`AeroCowDisk` composes a read-only base disk with a writable sparse overlay. Reads hit
the overlay when the block is allocated, otherwise fall back to the base. Writes always
go to the overlay.
