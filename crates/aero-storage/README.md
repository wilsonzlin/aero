# `aero-storage`

This crate contains Aero’s virtual disk abstractions and pure Rust disk image formats.

Browser persistence backends (OPFS + IndexedDB) live in the TypeScript host layer, but
the formats here are designed to be used directly with those backends.

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

