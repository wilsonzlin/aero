# `aero-storage`

This crate contains Aero’s virtual disk abstractions and pure Rust disk image formats.

In the browser, the primary persistence backend is OPFS. Aero provides a Rust/wasm32
implementation in `crates/aero-opfs` (e.g. `OpfsByteStorage`) that implements
`aero_storage::StorageBackend`/`aero_storage::VirtualDisk`.

Higher-level orchestration such as remote HTTP streaming/caching and UI integration may
still live in the TypeScript host layer.

Note: IndexedDB-based storage is generally async and is not currently exposed as a sync
`aero_storage::StorageBackend`.

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

## Using `aero-storage` disks with device models

Device models such as NVMe and virtio-blk live in separate crates and generally use their own
disk traits. To avoid duplicating disk abstractions, the workspace provides adapters in
`crates/aero-storage-adapters/` so an [`aero_storage::VirtualDisk`] can be used as a backend for:

- `aero-devices-nvme` (`DiskBackend`)
- `aero-devices` virtio-blk (`storage::DiskBackend`)
- legacy `emulator` storage models (`io::storage::disk::DiskBackend`)

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
