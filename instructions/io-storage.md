# Workstream D: I/O & Storage

> **⚠️ MANDATORY: Read and follow [`AGENTS.md`](../AGENTS.md) in its entirety before starting any work.**
>
> AGENTS.md contains critical operational guidance including:
> - Defensive mindset (assume hostile/misbehaving code)
> - Resource limits and `safe-run.sh` usage
> - Windows 7 test ISO location (`/state/win7.iso`)
> - Interface contracts
> - Technology stack decisions
>
> **Failure to follow AGENTS.md will result in broken builds, OOM kills, and wasted effort.**

---

## Overview

This workstream owns **storage emulation**: IDE/AHCI/NVMe disk controllers, disk image abstraction, and the browser storage backends (OPFS, IndexedDB).

Storage is on the **critical boot path**. Windows 7 cannot start without a working disk controller.

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-storage/` | Disk image abstraction, caching |
| `crates/aero-storage-server/` | Storage server for remote images |
| `crates/aero-devices-storage/` | IDE/AHCI controller emulation |
| `crates/aero-devices-nvme/` | NVMe controller emulation |
| `crates/aero-opfs/` | Origin Private File System backend |
| `crates/aero-http-range/` | HTTP Range request handling |
| `crates/st-idb/` | IndexedDB async storage backend |
| `web/src/storage/` | TypeScript storage worker / host layer (orchestrates streaming/caching + hosts wasm backends) |

---

## Essential Documentation

**Must read:**

 - [`docs/05-storage-subsystem.md`](../docs/05-storage-subsystem.md) — Storage architecture
 - [`docs/05-storage-topology-win7.md`](../docs/05-storage-topology-win7.md) — Canonical Win7 storage topology (PCI BDFs + AHCI/IDE media mapping)
 - [`docs/20-storage-trait-consolidation.md`](../docs/20-storage-trait-consolidation.md) — Canonical disk/backend traits + consolidation plan
 - [`docs/19-indexeddb-storage-story.md`](../docs/19-indexeddb-storage-story.md) — IndexedDB (async) vs Rust controller (sync) integration plan
 - [`docs/16-remote-disk-image-delivery.md`](../docs/16-remote-disk-image-delivery.md) — Remote disk streaming
 - [`docs/18-chunked-disk-image-format.md`](../docs/18-chunked-disk-image-format.md) — Chunked format

**Reference:**

- [`docs/17-range-cdn-behavior.md`](../docs/17-range-cdn-behavior.md) — CDN Range request behavior
- [`docs/16-disk-image-streaming-auth.md`](../docs/16-disk-image-streaming-auth.md) — Auth for streaming
- [`docs/backend/disk-image-streaming-service.md`](../docs/backend/disk-image-streaming-service.md) — Backend service

---

## Tasks

### Storage Controller Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| ST-001 | IDE controller emulation | P0 | None | High |
| ST-002 | AHCI controller emulation | P0 | None | Very High |
| ST-003 | Disk image abstraction | P0 | None | Medium |
| ST-004 | OPFS backend | P0 | ST-003 | Medium |
| ST-005 | IndexedDB fallback backend | P1 | ST-003 | Medium |
| ST-006 | Sector caching | P1 | ST-003 | Medium |
| ST-007 | Sparse disk format | P1 | ST-003 | Medium |
| ST-008 | CD-ROM/ATAPI emulation | P0 | ST-001 | High |
| ST-009 | Virtio-blk device model | P1 | VTP-001..VTP-003 | High |
| ST-010 | Storage test suite | P0 | ST-002 | Medium |

Note: IndexedDB-based storage is async and is not currently exposed as a synchronous
`aero_storage::StorageBackend` / `aero_storage::VirtualDisk`. See
[`docs/19-indexeddb-storage-story.md`](../docs/19-indexeddb-storage-story.md) and the canonical trait
mapping in [`docs/20-storage-trait-consolidation.md`](../docs/20-storage-trait-consolidation.md).

### NVMe Tasks

NVMe is an optional high-performance path. AHCI is sufficient for Windows 7.

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| NV-001 | NVMe controller registers | P2 | None | High |
| NV-002 | Admin queue implementation | P2 | NV-001 | High |
| NV-003 | I/O queue implementation | P2 | NV-002 | High |
| NV-004 | NVMe test suite | P2 | NV-003 | Medium |

---

## Storage Architecture

### Layered Design

```
┌─────────────────────────────────────────────┐
│            Windows 7 Guest                   │
│                 │                            │
│     AHCI Driver / Virtio-blk Driver         │
├─────────────────┼───────────────────────────┤
│                 ▼                            │
│        AHCI Controller Emulation            │  ← ST-002
│        Virtio-blk Device Model              │  ← ST-009
│                 │                            │
│                 ▼                            │
│         Disk Image Abstraction              │  ← ST-003
│                 │                            │
│         ┌───────┴───────┐                   │
│         ▼               ▼                   │
│      OPFS Backend    HTTP Range             │  ← ST-004
│      (local)         (remote streaming)     │
└─────────────────────────────────────────────┘
```

### Remote Streaming

For large disk images (20GB+), we stream sectors on demand:

```
Browser
    │
    ▼
HTTP Range GET → CDN → Origin (S3/R2/etc.)
    │
    ▼
Sector Cache (in-memory + OPFS)
    │
    ▼
AHCI Controller
```

Key considerations:
- HTTP Range requests for random access
- Sector-level caching to minimize network requests
- CORS and COEP headers for cross-origin isolation
- Authentication for private disk images

---

## AHCI Implementation Notes

AHCI (Advanced Host Controller Interface) is the SATA controller standard. Key components:

1. **HBA Memory Registers** — PCI BAR5 MMIO
2. **Port Registers** — Per-port command/status
3. **Command List** — Ring of command headers
4. **FIS (Frame Information Structure)** — Data transfer descriptors

Windows 7 uses the inbox `msahci.sys` driver.

Reference: AHCI 1.3.1 specification (publicly available from Intel).

---

## OPFS Backend

Origin Private File System provides fast, large file access in the browser.

In this repo, the primary OPFS backend implementation lives in Rust/wasm32 in
`crates/aero-opfs` (e.g. `aero_opfs::OpfsByteStorage` / `aero_opfs::OpfsBackend`),
which calls the underlying browser OPFS APIs via `wasm-bindgen`.

The TypeScript host layer is still responsible for wiring the worker runtime and may
orchestrate higher-level concerns like remote streaming and cache policy.

Underlying browser API (for reference):

```typescript
// Get OPFS root
const root = await navigator.storage.getDirectory();

// Create/open disk image file
const fileHandle = await root.getFileHandle('disk.img', { create: true });

// Get sync access handle for random access
const accessHandle = await fileHandle.createSyncAccessHandle();

// Read sector
const buffer = new ArrayBuffer(512);
accessHandle.read(buffer, { at: sectorOffset });

// Write sector
accessHandle.write(data, { at: sectorOffset });
```

Rust usage (wasm32):

```rust
use aero_opfs::OpfsByteStorage;
use aero_storage::StorageBackend;

let mut backend = OpfsByteStorage::open("disk.img", true).await?;

let mut sector = [0u8; 512];
backend.read_at(0, &mut sector)?;
backend.write_at(0, &sector)?;
backend.flush()?;
```

---

## Coordination Points

### Dependencies on Other Workstreams

- **CPU (A)**: AHCI registers accessed via `CpuBus`
- **Integration (H)**: Controller must be wired into PCI bus

### What Other Workstreams Need From You

- Working AHCI for Windows 7 boot
- CD-ROM for ISO booting
- Virtio-blk device model for driver development (C)

---

## Testing

QEMU boot integration tests live under the workspace root `tests/` directory, but are registered
under the `emulator` crate via `crates/emulator/Cargo.toml` `[[test]]` entries (e.g.
`path = "../../tests/freedos_boot.rs"`). Always run them via `-p emulator` (not `-p aero`).

```bash
# Run storage tests
bash ./scripts/safe-run.sh cargo test -p aero-storage --locked
bash ./scripts/safe-run.sh cargo test -p aero-devices-storage --locked
bash ./scripts/safe-run.sh cargo test -p aero-opfs --locked

# Integration tests
# Note: the first run in a clean/contended agent sandbox can take >10 minutes to compile.
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p emulator --test freedos_boot --locked
```

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `bash ./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/05-storage-subsystem.md`](../docs/05-storage-subsystem.md)
4. ☐ Explore `crates/aero-storage/src/` and `crates/aero-devices-storage/src/`
5. ☐ Run existing tests to establish baseline
6. ☐ Pick a task from the tables above and begin

---

*Storage is the foundation. Nothing works without a working disk.*
