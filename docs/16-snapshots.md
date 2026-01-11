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

### Core sections (format v1)

| Section | Contents |
|--------:|----------|
| `META` | Snapshot id, parent id, timestamp, optional label |
| `CPU` | Architectural CPU state (v1: minimal; v2: `aero_cpu_core::state::CpuState` compatible) |
| `CPUS` | Multi-vCPU CPU state (v1: minimal; v2: `aero_cpu_core::state::CpuState` compatible) |
| `MMU` | System/MMU state (v1: minimal; v2: control/debug/MSR + descriptor tables) |
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

## CPU section encoding

### CPU section v1 (section_version = 1)

Legacy encoding used by early/minimal VMs. Only includes:

- 16 GPRs (u64), RIP (u64), RFLAGS (u64)
- Segment selectors only (CS/DS/ES/FS/GS/SS as u16)
- XMM0-15 (u128)

### CPU section v2 (section_version = 2)

Supports snapshot/restore for the `aero_cpu_core::state::CpuState` execution core.

All fields are written **in the order below** (little-endian):

- **GPRs (u64 each)** in architectural order: `RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8..R15`
- `u64 RIP`
- `u64 RFLAGS` (materialized; no lazy-flags encoding)
- `u8 CPU_MODE` (`0=Real, 1=Protected, 2=Long, 3=Vm86`)
- `u8 HALTED` (0/1)
- **SegmentState** for `ES, CS, SS, DS, FS, GS` (in that order), each:
  - `u16 selector`
  - `u64 base`
  - `u32 limit`
  - `u32 access` (VMX-style "AR bytes" + `unusable` bit at 16)
- **x87/FPU state**:
  - `u16 fcw`
  - `u16 fsw`
  - `u16 ftw`
  - `u8 top`
  - `u16 fop`
  - `u64 fip`
  - `u64 fdp`
  - `u16 fcs`
  - `u16 fds`
  - `u128 st[8]` (ST0..ST7 FXSAVE slots)
- **SSE state**:
  - `u32 MXCSR`
  - `u128 XMM[16]` (XMM0..XMM15)
- `u8 FXSAVE_AREA[512]` (raw 512-byte image; currently `FXSAVE64`-compatible layout)

---

## MMU section encoding

### MMU section v1 (section_version = 1)

Legacy encoding:

- `u64 CR0, CR2, CR3, CR4, CR8`
- `u64 EFER`
- `u64 GDTR_BASE`, `u16 GDTR_LIMIT`
- `u64 IDTR_BASE`, `u16 IDTR_LIMIT`

### MMU section v2 (section_version = 2)

All fields are written **in the order below**:

- `u64 CR0, CR2, CR3, CR4, CR8`
- `u64 DR0, DR1, DR2, DR3, DR4, DR5, DR6, DR7`
- **MSRs (u64 each)**:
  - `EFER`
  - `STAR, LSTAR, CSTAR, SFMASK`
  - `SYSENTER_CS, SYSENTER_EIP, SYSENTER_ESP`
  - `FS_BASE, GS_BASE, KERNEL_GS_BASE`
  - `APIC_BASE`
  - `TSC`
- `u64 GDTR_BASE`, `u16 GDTR_LIMIT`
- `u64 IDTR_BASE`, `u16 IDTR_LIMIT`
- **SegmentState** `LDTR` then `TR` (same layout as CPU v2 SegmentState)

---

## CPU internal (device) encoding

Some CPU execution state is not part of the architectural `CPU`/`MMU` sections (e.g. pending
external interrupts). If needed, it is stored in the `DEVICES` section as a device entry:

- `DeviceId::CPU_INTERNAL` (`9`)

### CPU_INTERNAL device v2 (device_version = 2)

- `u8 interrupt_inhibit` (interrupt shadow / inhibit counter)
- `u32 pending_len`
- `u8 pending_external_interrupts[pending_len]`

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
