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

- `docs/05-storage-subsystem.md` (subsystem overview)
- `docs/19-indexeddb-storage-story.md` (the async-vs-sync browser constraint analysis)

---

## Inventory: disk/backing-store traits in-tree

> Links are to the defining source files.

### Rust (synchronous)

- `aero_storage::StorageBackend` (sync, **byte-addressed**, resizable backend)\
  Defined in: [`crates/aero-storage/src/backend.rs`](../crates/aero-storage/src/backend.rs)
- `aero_storage::VirtualDisk` (sync, fixed-capacity virtual disk; byte addressing + sector helpers)\
  Defined in: [`crates/aero-storage/src/disk.rs`](../crates/aero-storage/src/disk.rs)
- `aero_devices::storage::DiskBackend` (sync, byte-addressed device-model backend used by virtio-blk etc)\
  Defined in: [`crates/devices/src/storage/mod.rs`](../crates/devices/src/storage/mod.rs)
- `aero_devices_nvme::DiskBackend` (sync, **sector-addressed** backend used by the NVMe controller model)\
  Defined in: [`crates/aero-devices-nvme/src/lib.rs`](../crates/aero-devices-nvme/src/lib.rs)
- `aero_io_snapshot::io::storage::state::DiskBackend` (sync, byte-addressed backend used by the snapshot layer)\
  Defined in: [`crates/aero-io-snapshot/src/io/storage/state.rs`](../crates/aero-io-snapshot/src/io/storage/state.rs)

### Rust (legacy, synchronous)

- `emulator::io::storage::disk::ByteStorage` (legacy, sync, byte-addressed backend)\
  Defined in: [`crates/emulator/src/io/storage/disk.rs`](../crates/emulator/src/io/storage/disk.rs)
- `emulator::io::storage::disk::DiskBackend` (legacy, sync, sector-addressed backend)\
  Defined in: [`crates/emulator/src/io/storage/disk.rs`](../crates/emulator/src/io/storage/disk.rs)

### Rust (asynchronous, browser-oriented)

- `st_idb::io::storage::DiskBackend` (async, byte-addressed backend used by the IndexedDB block store + cache)\
  Defined in: [`crates/st-idb/src/io/storage/mod.rs`](../crates/st-idb/src/io/storage/mod.rs)

### TypeScript (asynchronous, browser-oriented)

- `AsyncSectorDisk` (async, sector-addressed)\
  Defined in: [`web/src/storage/disk.ts`](../web/src/storage/disk.ts)

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
- `aero_opfs::OpfsByteStorage` (browser OPFS SyncAccessHandle; wasm32 only)

### Layer 3 (disk image formats): canonical = `aero_storage::VirtualDisk` (+ `StorageBackend`)

Disk image formats and disk “wrappers” should live in `crates/aero-storage` and should expose a
fixed-capacity, random-access disk via `aero_storage::VirtualDisk`.

Rule of thumb:

- If you are implementing a **disk image format** (raw/qcow2/vhd/sparse/overlay/cache), implement
  `VirtualDisk` and (if it needs a resizable backing store) be generic over `StorageBackend`.
- Do **not** introduce new format-specific “disk traits” in other crates.

### Layer 4 (synchronous device/controller models): canonical = `aero_storage::VirtualDisk`

New synchronous Rust device/controller models should treat `aero_storage::VirtualDisk` as the
canonical “disk” trait.

Some device crates currently define their own `DiskBackend` traits for historical reasons (custom
error types, sector-size reporting, or different mutability requirements). Those traits should be
treated as **device-internal integration traits**, and code should prefer accepting a
`Box<dyn aero_storage::VirtualDisk>` at public boundaries unless there is a concrete reason not to.

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
- `impl aero_devices_nvme::DiskBackend for AeroVirtualDiskAsNvmeBackend`: in `crates/aero-devices-nvme`
- `impl aero_devices::storage::DiskBackend for AeroVirtualDiskAsDeviceBackend`: in `crates/devices`

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

### Phase 3: deprecate and remove legacy emulator traits

1. Migrate `crates/emulator` internal call sites from:
   - `ByteStorage` → `aero_storage::StorageBackend`
   - `DiskBackend` → `aero_storage::VirtualDisk`
2. Remove the emulator traits once no longer used, keeping only any genuinely emulator-specific glue.

### Phase 4 (optional, future): IndexedDB as runtime fallback for sync controllers (Option C)

1. Implement the “storage worker” RPC protocol (shared memory + `Atomics`)
2. Implement `aero_storage::StorageBackend` for the RPC client
3. Reuse existing `aero_storage` disk formats + existing synchronous controllers unchanged

