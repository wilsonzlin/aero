# 20 - Storage trait consolidation (disk/backing-store traits)

## Context

This repo currently contains **multiple overlapping “disk/backing-store” traits** across Rust and
TypeScript. That has been useful during bring-up, but it also creates a constant risk that new work
accidentally introduces *yet another* incompatible trait instead of reusing existing ones.

This document is the repo’s **source of truth** for:

1. What disk/backing-store traits exist today (inventory, with links)
2. Which traits are **canonical** for each layer
3. How adapters should be structured (where the wrapper types vs `impl Trait for ...` live)
4. The browser story (sync vs async, IndexedDB deadlock constraints)
5. A phased plan to converge, especially for `crates/emulator`

See also:

- [`05-storage-subsystem.md`](./05-storage-subsystem.md) (subsystem overview)
- [`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md) (the async-vs-sync browser constraint analysis)
- [`21-emulator-crate-migration.md`](./21-emulator-crate-migration.md) (`crates/emulator` → canonical stack plan + deletion targets)

---

## Inventory: disk/backing-store traits in-tree

> Links are to the defining source files.

### Rust (synchronous)

- `aero_storage::StorageBackend` (sync, **byte-addressed**, resizable backend)\
  Defined in: [`crates/aero-storage/src/backend.rs`](../crates/aero-storage/src/backend.rs)
- `aero_storage::StdFileBackend` / `aero_storage::FileBackend` (sync, byte-addressed native `std::fs::File` backend; non-wasm32)\
  Defined in: [`crates/aero-storage/src/backend.rs`](../crates/aero-storage/src/backend.rs)
- `aero_storage::VirtualDisk` (sync, fixed-capacity virtual disk; byte addressing + sector helpers)\
  Defined in: [`crates/aero-storage/src/disk.rs`](../crates/aero-storage/src/disk.rs)
- `aero_storage::streaming::ChunkStore` (sync, chunk-addressed cache store used by `aero_storage::StreamingDisk`)\
  Defined in: [`crates/aero-storage/src/streaming.rs`](../crates/aero-storage/src/streaming.rs)
- `aero_devices::storage::DiskBackend` (sync, byte-addressed device-model backend used by virtio-blk etc)\
  Defined in: [`crates/devices/src/storage/mod.rs`](../crates/devices/src/storage/mod.rs)
- `aero_devices_nvme::DiskBackend` (sync, **sector-addressed** backend used by the NVMe controller model)\
  Defined in: [`crates/aero-devices-nvme/src/lib.rs`](../crates/aero-devices-nvme/src/lib.rs)
- `aero_devices_storage::atapi::IsoBackend` (sync, read-only 2048-byte-sector CD/ISO backend for ATAPI)\
  Defined in: [`crates/aero-devices-storage/src/atapi.rs`](../crates/aero-devices-storage/src/atapi.rs)
- `aero_virtio::devices::blk::BlockBackend` (sync, byte-addressed backend used by the `aero-virtio` virtio-blk device model)\
  Defined in: [`crates/aero-virtio/src/devices/blk.rs`](../crates/aero-virtio/src/devices/blk.rs)
- `aero_io_snapshot::io::storage::state::DiskBackend` (sync, byte-addressed backend used by the snapshot layer)\
  Defined in: [`crates/aero-io-snapshot/src/io/storage/state.rs`](../crates/aero-io-snapshot/src/io/storage/state.rs)
- `aero_opfs::io::snapshot_file::OpfsSyncFileHandle` (sync, byte-addressed file-handle interface used to adapt OPFS `SyncAccessHandle` to `std::io::{Read, Write, Seek}`)\
  Defined in: [`crates/aero-opfs/src/io/snapshot_file.rs`](../crates/aero-opfs/src/io/snapshot_file.rs)
- `firmware::bios::BlockDevice` (sync, 512-byte-sector read-only block device interface used by the legacy BIOS INT 13h implementation)\
  Defined in: [`crates/firmware/src/bios/mod.rs`](../crates/firmware/src/bios/mod.rs)

### Rust (legacy, synchronous)

- `emulator::io::storage::disk::ByteStorage` (legacy, sync, byte-addressed backend)\
  Defined in: [`crates/emulator/src/io/storage/disk.rs`](../crates/emulator/src/io/storage/disk.rs)
- `emulator::io::storage::disk::DiskBackend` (legacy, sync, sector-addressed backend)\
  Defined in: [`crates/emulator/src/io/storage/disk.rs`](../crates/emulator/src/io/storage/disk.rs)

### Rust (asynchronous, browser-oriented)

- `st_idb::io::storage::DiskBackend` (async, byte-addressed backend used by the IndexedDB block store + cache)\
  Defined in: [`crates/st-idb/src/io/storage/mod.rs`](../crates/st-idb/src/io/storage/mod.rs)

### Rust (asynchronous, server-side)

- `aero_storage_server::store::ImageStore` (async, byte-stream access for serving disk images over HTTP)\
  Defined in: [`crates/aero-storage-server/src/store/mod.rs`](../crates/aero-storage-server/src/store/mod.rs)

### TypeScript (asynchronous, browser-oriented)

- `AsyncSectorDisk` (async, sector-addressed)\
  Defined in: [`web/src/storage/disk.ts`](../web/src/storage/disk.ts)
- `SparseBlockDisk` (async, sector-addressed + fixed-size block operations for sparse/overlay disks)\
  Defined in: [`web/src/storage/sparse_block_disk.ts`](../web/src/storage/sparse_block_disk.ts)
- `RemoteRangeDiskSparseCache` / `RemoteRangeDiskSparseCacheFactory` / `RemoteRangeDiskMetadataStore` (async, remote Range disk cache abstractions)\
  Defined in: [`web/src/storage/remote_range_disk.ts`](../web/src/storage/remote_range_disk.ts)
- `BinaryStore` (async, byte store abstraction used by the chunked remote disk implementation)\
  Defined in: [`web/src/storage/remote_chunked_disk.ts`](../web/src/storage/remote_chunked_disk.ts)
- `DiskAccessLease` (async, refreshable “signed URL lease” used by remote disk readers)\
  Defined in: [`web/src/storage/disk_access_lease.ts`](../web/src/storage/disk_access_lease.ts)
- `RemoteCacheDirectoryHandle` / `RemoteCacheFileHandle` / `RemoteCacheFile` / `RemoteCacheWritableFileStream` (async, OPFS-like handle abstractions used by the remote cache manager)\
  Defined in: [`web/src/storage/remote_cache_manager.ts`](../web/src/storage/remote_cache_manager.ts)
- `RemoteChunkCacheBackend` (async, chunk cache backend interface for OPFS LRU chunk caches)\
  Defined in: [`web/src/storage/remote/opfs_lru_chunk_cache.ts`](../web/src/storage/remote/opfs_lru_chunk_cache.ts)
- `AeroIpcIoDispatchTarget` (AIPC I/O server dispatch interface; includes optional disk read/write RPC hooks)\
  Defined in: [`web/src/io/ipc/aero_ipc_io.ts`](../web/src/io/ipc/aero_ipc_io.ts)

---

## Intended layering (what to use where)

The storage stack is easiest to reason about when split into explicit layers:

1. **Host persistence** (OPFS file, IndexedDB, in-memory, network)
2. **Byte backend** (random access, resize)
3. **Disk image formats / disk wrappers** (raw, sparse, qcow2, VHD, block cache, COW overlays)
4. **Device/controller integration** (AHCI/IDE/NVMe/virtio-blk consuming a “disk”)

### Layer 2 (byte backend): canonical = `aero_storage::StorageBackend`

Use `aero_storage::StorageBackend` as the canonical Rust trait for **synchronous byte-addressed**
storage (files, buffers, OPFS sync access handles).

This is the trait that disk image formats should be generic over.

Examples of implementations:

- `aero_storage::MemBackend` (tests)
- `aero_storage::FileBackend` / `aero_storage::StdFileBackend` (native filesystem backend; non-wasm32)\
  Used by host-side tooling such as [`tools/aero-disk-convert`](../tools/aero-disk-convert/src/main.rs)
  and CLI/image tests.
- `aero_opfs::OpfsByteStorage` (browser OPFS SyncAccessHandle; wasm32 only)

### Layer 3 (disk image formats): canonical = `aero_storage::VirtualDisk` (+ `StorageBackend`)

Disk image formats and disk “wrappers” should live in `crates/aero-storage` and should expose a
fixed-capacity, random-access disk via `aero_storage::VirtualDisk`.

Rule of thumb:

- If you are implementing a **disk image format** (raw/qcow2/vhd/sparse/overlay/cache), implement
  `VirtualDisk` and (if it needs a resizable backing store) be generic over `StorageBackend`.
- Do **not** introduce new format-specific “disk traits” in other crates.

#### Read-only wrappers (when and why)

Aero frequently attaches media that is **semantically immutable**:

- **Base OS images** (golden Windows installs, demo images)
- **Install/driver ISOs** attached via ATAPI
- **Remote “chunked” base images** served from object storage/CDNs

For these, prefer to enforce immutability in the **disk/backing-store layer** with a read-only
wrapper (e.g. `aero_storage::ReadOnlyDisk` / `aero_storage::ReadOnlyBackend` around the canonical
traits), rather than relying on “we just won’t call `write_at`”.

This catches accidental write paths early (and makes bugs loud), and it documents intent at the API
boundary.

Typical composition for a writable VM disk built on a read-only base:

1. **Base (read-only)**: `VirtualDisk` backed by a remote image, a local file, or an unpacked format
   (qcow2/vhd/…)
2. **Writeback overlay (writable)**: `aero_storage::AeroCowDisk` or a sparse disk overlay
3. **Optional cache**: `aero_storage::BlockCachedDisk`
4. **Device integration**: device/controller consumes `Box<dyn aero_storage::VirtualDisk>`

#### Aero sparse formats: `AEROSPAR` vs legacy `AEROSPRS`

- `AEROSPAR` (“AeroSparse”, magic `AEROSPAR`) is the **current** sparse disk format implemented in
  `crates/aero-storage` as `aero_storage::AeroSparseDisk` in
  [`crates/aero-storage/src/sparse.rs`](../crates/aero-storage/src/sparse.rs).
  `crates/emulator` uses it via a thin wrapper (`AerosparDisk`) in
  [`crates/emulator/src/io/storage/formats/aerospar.rs`](../crates/emulator/src/io/storage/formats/aerospar.rs).
- `AEROSPRS` (magic `AEROSPRS`) is a **legacy** sparse format still implemented in the emulator:
  [`crates/emulator/src/io/storage/formats/aerosprs.rs`](../crates/emulator/src/io/storage/formats/aerosprs.rs)
  (selected via [`crates/emulator/src/io/storage/formats/sparse.rs`](../crates/emulator/src/io/storage/formats/sparse.rs)).
- Why it exists: open/migrate older images created before `AEROSPAR` became canonical.
- Differences: `AEROSPAR` has a 64‑byte header + simple u64 allocation table; `AEROSPRS` uses a 4 KiB
  header with explicit sector size (512/4096) plus a small journal for crash‑safe table updates.
- Status/limits: `AEROSPRS` is **emulator-only** and not used by the new controller stack.
  `crates/emulator` creates only `AEROSPAR` (`SparseDisk::create`); `AEROSPRS` is supported only for
  opening/mutating legacy images. New work should create/consume `AEROSPAR` via `crates/aero-storage`.
- Migration: see the offline converter tool
  [`crates/emulator/src/bin/aerosparse_convert.rs`](../crates/emulator/src/bin/aerosparse_convert.rs).
- Tests (Task 84): [`crates/emulator/tests/storage_formats.rs`](../crates/emulator/tests/storage_formats.rs)
  (`detect_aerosprs_*`) and [`crates/aero-storage/tests/storage_formats.rs`](../crates/aero-storage/tests/storage_formats.rs).

### Layer 4 (synchronous device/controller models): canonical = `aero_storage::VirtualDisk`

New synchronous Rust device/controller models should treat `aero_storage::VirtualDisk` as the
canonical “disk” trait.

Some device crates currently define their own `DiskBackend` traits for historical reasons (custom
error types, sector-size reporting, or different mutability requirements). Those traits should be
treated as **device-internal integration traits**, and code should prefer accepting a
`Box<dyn aero_storage::VirtualDisk>` at public boundaries unless there is a concrete reason not to.

In particular, virtio-blk device models may expose crate-local backend traits
(`aero_devices::storage::DiskBackend`, `aero_virtio::devices::blk::BlockBackend`). Treat those
traits as *device-internal*; “platform wiring” should prefer `aero_storage::VirtualDisk` and adapt
at the device boundary.

When a device crate *must* keep its own trait, the preferred integration pattern is:

- Accept the device’s trait internally (e.g. `aero_devices_nvme::DiskBackend`)
- Provide an adapter from `aero_storage::VirtualDisk` at the API boundary (e.g.
  `aero_devices_nvme::from_virtual_disk`)

---

## Adapter structure (important: where code should live)

### Wrapper types live in `crates/aero-storage-adapters`

`crates/aero-storage-adapters` provides wrapper **types** around `aero_storage::VirtualDisk`
(e.g. mutex/refcell wrappers, alignment enforcement, helper accessors).

This avoids duplicating the same wrapper type across multiple device crates.

### Trait `impl`s live in the trait’s owning crate (Rust orphan rules)

Because of Rust’s orphan rules, `impl SomeExternalTrait for SomeExternalType` must live in a crate
that defines either the trait or the type.

Therefore:

- `aero-storage-adapters` should generally **not** implement `aero_devices::*::DiskBackend` traits.
- The crate that defines the trait (e.g. `aero-devices-nvme`, `aero-devices`) should implement its
  trait for the wrapper types provided by `aero-storage-adapters`.

This pattern is already used today:

 - Wrapper types: `crates/aero-storage-adapters`
 - `impl aero_devices_nvme::DiskBackend for AeroVirtualDiskAsNvmeBackend` (re-exported as
   `aero_devices_nvme::AeroStorageDiskAdapter`): in `crates/aero-devices-nvme`
 - `impl aero_devices::storage::DiskBackend for AeroVirtualDiskAsDeviceBackend` (re-exported as
   `aero_devices::storage::AeroStorageDiskAdapter`): in `crates/devices`
 - `impl<T: aero_storage::VirtualDisk> aero_virtio::devices::blk::BlockBackend for Box<T>` (plus impls
   for `Box<dyn VirtualDisk>` / `Box<dyn VirtualDisk + Send>`): in `crates/aero-virtio`
 - Canonical disk-backed virtio-blk device type: `aero_virtio::devices::blk::VirtioBlkDisk` (type
   alias for `VirtioBlk<Box<dyn VirtualDisk>>` on wasm32, and `VirtioBlk<Box<dyn VirtualDisk + Send>>`
   on native): in `crates/aero-virtio`
 - Reverse adapter: `crates/devices/src/storage/mod.rs` defines `DeviceBackendAsAeroVirtualDisk`, which
   allows reusing `aero-storage` disk wrappers (cache/sparse/COW) on top of an existing
   `aero_devices::storage::DiskBackend`.
 - Reverse adapter: `crates/aero-virtio/src/devices/blk.rs` defines
   `aero_virtio::devices::blk::BlockBackendAsAeroVirtualDisk`, which allows reusing `aero-storage`
   disk wrappers on top of an existing `aero_virtio::devices::blk::BlockBackend`.
 - Reverse adapter: `crates/aero-devices-nvme/src/nvme_as_aero_storage.rs` defines
   `aero_devices_nvme::NvmeBackendAsAeroVirtualDisk`, which allows reusing `aero-storage` disk
   wrappers on top of an existing `aero_devices_nvme::DiskBackend`.
 - BIOS/firmware bridge: `crates/aero-machine/src/shared_disk.rs` defines `SharedDisk`, a wrapper type
   that implements both `firmware::bios::BlockDevice` and `aero_storage::VirtualDisk` so a single
   disk image can be used consistently across the “boot firmware” and “PCI storage controller”
   phases.

If a new device crate needs a disk backend trait, it should follow the same structure:

1. Prefer `aero_storage::VirtualDisk` directly
2. If a crate-specific trait is still needed, add:
   - a wrapper type (if required) in `aero-storage-adapters`
   - the trait `impl` in the device crate

---

## Browser story (sync vs async): Option A vs Option C

This repo has to bridge two realities:

- Rust device/controller models are currently **synchronous**
- IndexedDB (and many other browser APIs) are fundamentally **asynchronous**

`docs/19-indexeddb-storage-story.md` explains why “pretend IndexedDB is sync” deadlocks if attempted
from the same Worker.

### Option A (near-term default): require OPFS SyncAccessHandle for the Rust controller path

Use OPFS `FileSystemSyncAccessHandle` (worker-only) for boot-critical synchronous disk I/O.
IndexedDB remains available for **async-only host layer** features:

- disk manager metadata and UI
- import/export staging
- benchmarks/diagnostics
- snapshots saved outside the boot-critical hot path

This is the current recommendation in `docs/19-indexeddb-storage-story.md`.

### Option C (future / when truly needed): dedicated “storage worker” + sync RPC client

If we need IndexedDB to be a runtime fallback for the synchronous Rust controller path, the only
viable approach is Option C:

- Run IndexedDB I/O on a **separate worker** (async)
- Expose a synchronous facade to the controller worker via a shared-memory RPC protocol

This repo already has building blocks for such shared-memory “sync RPC” in the web runtime (AIPC
ring buffers + `Atomics.wait`), e.g. `web/src/io/ipc/aero_ipc_io.ts`. Option C should reuse those
primitives rather than introducing a new parallel IPC mechanism.

Under Option C, the end goal is still to implement the **canonical sync traits**
(`aero_storage::StorageBackend` / `aero_storage::VirtualDisk`) on top of the RPC client.

This avoids introducing new “special IndexedDB sync traits” and keeps the controller code unchanged.

---

## Canonical traits (quick reference)

- Disk image formats (Rust): **`aero_storage::{StorageBackend, VirtualDisk}`**
- Synchronous device/controller models (Rust): **`aero_storage::VirtualDisk`** (adapt as needed)
- Async browser host layer:
  - Rust/wasm helper crates (IndexedDB cache): **`st_idb::io::storage::DiskBackend`**
  - TypeScript runtime disks: **`AsyncSectorDisk`**

---

## Phased migration plan (to reduce/remove legacy traits)

This is intentionally a **sequence of small PRs** (no repo-wide flag day).

### Phase 0 (this PR): document + guardrails

1. Add this doc (`docs/20-storage-trait-consolidation.md`)
2. Add doc-comments on legacy traits pointing to the canonical traits
3. Add warnings on “fallback” constructors that may produce async-only backends (browser)

### Phase 1: converge disk image formats on `crates/aero-storage`

Goal: ensure there is exactly one place in-tree implementing raw/qcow2/vhd/sparse/overlay logic.

1. In `crates/emulator`, replace direct implementations of disk formats with thin wrappers around
   `crates/aero-storage` equivalents, using the existing adapters in
   `crates/emulator/src/io/storage/adapters.rs`.
2. Keep the existing emulator public API temporarily (type wrappers), but stop duplicating format logic.

### Phase 2: converge device/controller models on `aero_storage::VirtualDisk`

1. Prefer passing `Box<dyn aero_storage::VirtualDisk>` through “platform wiring” layers.
2. Limit crate-specific `DiskBackend` traits to truly internal needs.
3. Keep `crates/aero-storage-adapters` as the shared home for adapter wrapper *types*.
4. Standardize on consistent adapter naming and provide adapters in both directions:
   - `VirtualDisk` → device backend: device crates typically re-export the canonical wrapper types
     as `AeroStorageDiskAdapter` (e.g. `aero_devices::storage::AeroStorageDiskAdapter`,
     `aero_devices_nvme::AeroStorageDiskAdapter`).
   - Device backend → `VirtualDisk`: reverse adapters live in the device crates (e.g.
     `aero_virtio::devices::blk::BlockBackendAsAeroVirtualDisk`,
     `aero_devices::storage::DeviceBackendAsAeroVirtualDisk`,
     `aero_devices_nvme::NvmeBackendAsAeroVirtualDisk`).
5. virtio-blk: keep `aero_storage::VirtualDisk` as the wiring boundary and treat backend traits as
   device-internal (adapt at the edge). Concretely, prefer wiring:
    - `aero_virtio::devices::blk::VirtioBlkDisk` (canonical disk-backed virtio-blk device type alias), or
    - `aero_devices::storage::VirtualDrive::{new_from_aero_virtual_disk, try_new_from_aero_virtual_disk}`
      (for the `aero-devices` stack; prefer `try_new_*` when accepting arbitrary disks)

   (Optional cleanup) Evaluate consolidating virtio-blk device models on fewer backend traits:
    - `aero_devices::storage::DiskBackend`
    - `aero_virtio::devices::blk::BlockBackend`
    In both cases, keep `aero_storage::VirtualDisk` as the common “disk image” boundary and adapt.

### Phase 3: deprecate and remove legacy emulator traits

1. Migrate `crates/emulator` internal call sites from:
   - `ByteStorage` → `aero_storage::StorageBackend`
   - `DiskBackend` → `aero_storage::VirtualDisk`
   - `ide::atapi::IsoBackend` → `aero_devices_storage::atapi::IsoBackend` (or directly to `VirtualDisk`
     via a small adapter)
2. Remove the emulator traits once no longer used, keeping only any genuinely emulator-specific glue.

### Phase 4 (optional, future): IndexedDB as runtime fallback for sync controllers (Option C)

1. Implement the “storage worker” RPC protocol (shared memory + `Atomics`)
2. Implement `aero_storage::StorageBackend` for the RPC client
3. Reuse existing `aero_storage` disk formats + existing synchronous controllers unchanged
