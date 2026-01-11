# 16 - VM Snapshots (Save-State / Restore-State)

## Overview

Snapshots allow Aero to:

- Recover from crashes / tab closes (auto-save to persistent storage)
- Suspend/resume a running VM across page reloads
- Capture reproducible bug reports (attach a snapshot file + disk overlay references)

This repo contains a reference implementation of a versioned, forward-compatible snapshot format in `crates/aero-snapshot/` plus a minimal `Vm` integration in `crates/aero-vm/` with deterministic round-trip tests.

---

## Snapshot file format (v1)

### Global header

- **Magic:** `AEROSNAP` (8 bytes)
- **Format version:** `u16` (little-endian), currently `1`
- **Endianness tag:** `u8`, currently `1` for little-endian
- **Reserved:** `u8 + u32`

All integer fields in the format are **little-endian**.

### Section framing (TLV)

After the global header, the file is a sequence of sections:

- `u32 section_id`
- `u16 section_version`
- `u16 section_flags` (reserved)
- `u64 section_len`
- `section payload bytes (section_len)`

This framing enables **backward-compatible schema evolution**:

- Unknown `section_id`s can be skipped efficiently via `section_len`.
- New fields can be appended within a section payload without breaking old decoders.
- Per-section versions allow targeted upgrades without changing the whole file version.

### Deterministic encoding

`aero-snapshot` encodes snapshots **deterministically by default** to make snapshot bytes stable across runs for the same VM state:

- `DEVICES` entries are written in canonical order: `(device_id, device_version, device_flags)`.
- `DISKS` entries are written in canonical order: `disk_id`.
- Dirty-page RAM snapshots canonicalize the dirty page list: sorted ascending, deduplicated, and validated against the guest RAM size.

### Core sections (v1)

| Section | Contents |
|--------:|----------|
| `META` | Snapshot id, parent id, timestamp, optional label |
| `CPU` | Architectural CPU state (GPRs, RIP, RFLAGS, segment selectors, XMM regs) |
| `MMU` | Paging/descriptor-table state (CR0/2/3/4/8, EFER, GDTR/IDTR) |
| `DEVICES` | List of device states (PIC/APIC/PIT/RTC/PCI/Disk/VGA/etc) as typed TLVs |
| `DISKS` | References to disk base images + overlay images |
| `RAM` | Guest RAM contents (either full snapshot or dirty-page diff) |

### Recommended device payload convention (DEVICES)

`aero-snapshot` stores device entries as opaque `DeviceState { id, version, flags, data }`. For new device models, the recommended convention is:

- `DeviceState.id`: the *outer* Aero `DeviceId` (assigned by the coordinator)
- `DeviceState.data`: the raw `aero-io-snapshot` TLV blob returned by `IoSnapshot::save_state()` (including the inner header)
- `DeviceState.version` / `DeviceState.flags`: mirror the inner `SnapshotVersion` `(major, minor)` from the `aero-io-snapshot` header

This standardizes device payloads on a deterministic, forward-compatible TLV encoding. `aero-snapshot` provides opt-in helpers behind the `io-snapshot` feature: `aero_snapshot::io_snapshot_bridge::{device_state_from_io_snapshot, apply_io_snapshot_to_device}`.

---

## Guest RAM encoding

Guest RAM dominates snapshot size (multi-GB for Windows 7). `aero-snapshot` supports:

### 1) Full snapshot (chunked + optional compression)

- RAM is written as a stream of chunks (default: 1 MiB each)
- Each chunk is optionally compressed (currently LZ4)
- Restore applies chunks sequentially and does **not** require loading the whole snapshot into memory

### 2) Dirty-page diff snapshot (incremental)

- When the VM tracks dirty pages, a snapshot may include only modified pages since the last snapshot.
- Each page is stored as `(page_index, compressed_bytes)`.
- Restore applies diffs on top of an existing memory image (e.g., after loading a base full snapshot).
- **Important:** dirty-page snapshots require the RAM encoding `page_size` to match the VM's dirty
  tracking page size. `page_index` is measured in units of the dirty tracking granularity, so using
  a different `page_size` would corrupt offsets. `aero-snapshot` enforces this by requiring
  `SaveOptions.ram.page_size == SnapshotSource::dirty_page_size()` when `RamMode::Dirty`.
- Dirty snapshots are **not standalone**: they must only be applied on top of the snapshot they
  reference via `SnapshotMeta.parent_snapshot_id`.
  - Use `aero_snapshot::restore_snapshot_with_options` and pass
    `RestoreOptions { expected_parent_snapshot_id: Some(base_snapshot_id) }` to guard against
    accidentally applying a diff to the wrong base.
  - Full snapshots are standalone and ignore the expected-parent option.

---

## Storage integration (OPFS) + export/import

The intended browser persistence flow is:

1. **Auto-save** snapshot bytes to OPFS (Origin Private File System) on a timer.
2. On page load, attempt to load the most recent OPFS snapshot and restore the VM.
3. Provide:
   - **Export snapshot:** download the snapshot file (for bug reports).
   - **Import snapshot:** load a snapshot from a user-selected file.

OPFS access is via `navigator.storage.getDirectory()` and `FileSystemFileHandle` APIs (see `docs/05-storage-subsystem.md` for OPFS notes).

---

## UI expectations

The UI should expose:

- **Save** (manual snapshot)
- **Load** (restore snapshot)
- **Auto-save interval** (e.g., every N seconds; disabled when set to 0)

The JS host can store the snapshot in OPFS and/or trigger a download for export.

---

## Testing

The reference VM (`crates/aero-vm/`) contains deterministic tests:

- Run a deterministic program, snapshot mid-execution, restore into a fresh VM, and verify identical output + memory.
- Chain `full snapshot -> dirty diff snapshot` to validate incremental restore.

`crates/aero-snapshot/` additionally includes a `proptest`-based decoder robustness test that feeds random byte strings into the decoder and asserts it does not panic.
