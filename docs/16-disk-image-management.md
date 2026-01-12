# 16 - Disk Image Management (Browser)

## Overview

This repository now includes a minimal, browser-side **disk image manager** intended for real users:

- Create blank HDD images of a configurable size (e.g. 20–60GB).
- Import existing images (`.img`, `.iso`, `.qcow2`) via streaming `File.stream()` into OPFS (preferred) or IndexedDB (fallback).
- Import and convert common disk images (`.img`/raw, `.qcow2`, `.vhd`) into Aero’s internal sparse-on-OPFS format (`AEROSPAR`) via `DiskManager.importDiskConverted()` (OPFS-only).
- Export images as a `ReadableStream<Uint8Array>` (or via `DiskManager.exportDiskToFile()`), with optional gzip compression when `CompressionStream` is available.
- Persist metadata (JSON) per backend: name, kind (HDD/CD), size, last used, and a streaming CRC32 checksum.
- Maintain a mount selection: **one HDD (rw)** + **one CD (ro)** at minimum.

Note: IndexedDB storage is async and is used here as a host-side fallback for disk management and
import/export flows. It is not currently exposed as a synchronous `aero_storage::StorageBackend` /
`aero_storage::VirtualDisk` for the boot-critical Rust controller path (see
[`docs/19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md)).

Implementation lives in:

- `web/src/storage/disk_manager.ts` (main-thread API, worker client)
- `web/src/storage/disk_worker.ts` (worker implementation)
- `web/src/storage/import_export.ts` (streaming IO, OPFS/IDB)
- `web/src/storage/metadata.ts` (metadata + mounts)

## Storage layout

### OPFS backend

- Root: `navigator.storage.getDirectory()`
- Disk directory: `aero/disks/`
  - Images: `<diskId>.<ext>` (e.g. `.../aero/disks/<uuid>.img`)
  - Metadata: `aero/disks/metadata.json`

### IndexedDB backend

Single database: `aero-disk-manager`

- `disks` store: per-disk metadata
- `mounts` store: current mount selection
- `chunks` store: raw data chunks keyed by `[diskId, index]` (sparse; missing chunks are treated as zeros)

## Manual checklist (large import w/ progress)

1. Open the app with the disk manager UI (or a dev harness calling `DiskManager.importDisk()`).
2. Choose the OPFS backend if available.
3. Import a large image (multi-GB `.img` or `.iso`).
4. Verify:
   - Progress updates are displayed continuously (bytes processed increases).
   - The UI remains responsive during import (no main-thread hangs).
   - After import, the image appears in `DiskManager.listDisks()` with correct `sizeBytes`.
   - A checksum is recorded for imported images (`metadata.checksum.algorithm === "crc32"`).
5. Export the imported image:
   - Verify that export progress updates and the UI remains responsive.
   - Verify the exported file checksum matches metadata (or matches the original file if available).
