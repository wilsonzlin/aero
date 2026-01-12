# 16 - VM Snapshots (Save-State / Restore-State)

## Overview

Snapshots allow Aero to:

- Recover from crashes / tab closes (auto-save to persistent storage)
- Suspend/resume a running VM across page reloads
- Capture reproducible bug reports (attach a snapshot file + disk overlay references)

This repo contains a reference implementation of a versioned, forward-compatible snapshot format in `crates/aero-snapshot/` plus a minimal `Vm` integration in `crates/legacy/aero-vm/` with deterministic round-trip tests.

---

## Snapshot tooling (xtask)

During development it's often useful to sanity-check an `aero-snapshot` file without writing one-off Rust code or accidentally decompressing multi-GB RAM payloads.

The repo provides a small CLI via `xtask`:

```bash
# Print the header/META/section table + RAM encoding summary (no RAM decompression).
cargo xtask snapshot inspect path/to/snapshot.aerosnap

# Validate section framing and known section payloads (still no RAM decompression).
cargo xtask snapshot validate path/to/snapshot.aerosnap

# Fully restore the snapshot into a dummy target (includes RAM decompression).
# This mode refuses snapshots with >512MiB RAM as a safety limit.
cargo xtask snapshot validate --deep path/to/snapshot.aerosnap
```

On success, `validate` prints `valid snapshot` and exits 0.

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
- `CPUS` entries are written in canonical order: ascending `apic_id`.
- Dirty-page RAM snapshots canonicalize the dirty page list: sorted ascending, deduplicated, and validated against the guest RAM size.

### Validation / corruption handling

To keep restore behavior deterministic and avoid ambiguous state merges, the decoder treats the following as **corrupt**:

- Duplicate core sections (e.g. multiple `META`/`MMU`/`DEVICES`/`DISKS`/`RAM` sections).
- Multiple CPU sections (any mix of `CPU` + `CPUS`).
- Duplicate entries inside canonical lists:
  - `DEVICES`: duplicate `(device_id, version, flags)`
  - `DISKS`: duplicate `disk_id`
  - `CPUS`: duplicate `apic_id`
- Dirty RAM snapshots (`RAM` `mode = Dirty`) whose page index list is not **strictly increasing**.

### Core sections (format v1)

| Section | Contents |
|--------:|----------|
| `META` | Snapshot id, parent id, timestamp, optional label |
| `CPU` | Architectural CPU state (v1: minimal; v2: `aero_cpu_core::state::CpuState` compatible) |
| `CPUS` | Multi-vCPU CPU state (v1: minimal; v2: `aero_cpu_core::state::CpuState` compatible) |
| `MMU` | System/MMU state (v1: minimal; v2: control/debug/MSR + descriptor tables) |
| `DEVICES` | List of device states (PIC/APIC/PIT/RTC/I8042/HPET/ACPI_PM/PCI/Disk/VGA/etc) as typed TLVs |
| `DISKS` | References to disk base images + overlay images |
| `RAM` | Guest RAM contents (either full snapshot or dirty-page diff) |

### Recommended device payload convention (DEVICES)

`aero-snapshot` stores device entries as opaque `DeviceState { id, version, flags, data }`. For new device models, the recommended convention is:

- `DeviceState.id`: the *outer* Aero `DeviceId` (assigned by the coordinator)
- `DeviceState.data`: the raw `aero-io-snapshot` TLV blob returned by `IoSnapshot::save_state()` (including the inner header)
- `DeviceState.version` / `DeviceState.flags`: mirror the inner *device* `SnapshotVersion` from the `aero-io-snapshot` header (`version = device_major`, `flags = device_minor`, not the io-snapshot format version)

#### PcPlatform (full-system) device coverage

A minimal VM can sometimes get away with snapshotting just CPU/MMU + BIOS + RAM, but a full PcPlatform-based machine must also snapshot the platform devices that define interrupt/timer routing and input state.

Beyond BIOS/RAM, a PcPlatform snapshot is expected to include `DEVICES` entries for:

- **Platform interrupt controller complex** (legacy PIC + IOAPIC + LAPIC + IMCR routing)
- **Timers:** PIT, RTC, and HPET
- **ACPI PM fixed-function I/O** (SCI + PM_TMR, plus PM1/GPE state)
- **i8042 controller** (PS/2 keyboard/mouse controller and output-port state)

Restore notes:

- **HPET:** after restoring HPET state, call `Hpet::poll(&mut sink)` (or run one platform tick/poll iteration) to re-drive any pending **level-triggered** interrupt lines based on `general_int_status`. (`load_state()` intentionally does not directly touch the interrupt sink.)
- **ACPI PM determinism:** `PM_TMR` must be derived from deterministic virtual time for stable snapshot/restore. Avoid basing it on host wall-clock / `Instant` (which will make snapshots nondeterministic and can introduce time jumps on restore).

#### Common platform device ids (outer `DeviceId`)

Some platform devices are snapshotted as their own `DEVICES` entries and use dedicated `DeviceId` constants:

- `DeviceId::APIC` (`2`) — platform interrupt controller complex (`PlatformInterrupts`: legacy PIC + IOAPIC + LAPIC + IMCR routing). Historical id used by older snapshots.
- `DeviceId::PLATFORM_INTERRUPTS` (`21`) — preferred id for `PlatformInterrupts` snapshots (used by the canonical machine)
- `DeviceId::PIT` (`3`) — PIT (8254)
- `DeviceId::RTC` (`4`) — RTC/CMOS (0x70/0x71)
- `DeviceId::PCI` (`5`) — PCI core state (`PciCoreSnapshot` wrapper, inner `PCIC`, containing `PCPT` + `INTX`; see `PCI core state` below)
- `DeviceId::I8042` (`13`) — i8042 PS/2 controller (0x60/0x64)
- `DeviceId::PCI_CFG` (`14`) — legacy split-out PCI config I/O ports + PCI bus config-space image state (`PciConfigPorts`, inner `PCPT`)
- `DeviceId::PCI_INTX` (`15`) — legacy split-out PCI INTx router (`PciIntxRouter`, inner `INTX`)
- `DeviceId::ACPI_PM` (`16`) — ACPI power management registers (PM1 + PM timer)
- `DeviceId::HPET` (`17`) — HPET timer state
- `DeviceId::HDA` (`18`) — guest-visible HD Audio (HDA) controller/runtime state
- `DeviceId::E1000` (`19`) — Intel E1000 NIC model state

Note: `aero-snapshot` rejects duplicate `(DeviceId, version, flags)` tuples inside `DEVICES`. Since both `PciConfigPorts` and
`PciIntxRouter` currently snapshot as `SnapshotVersion (1.0)`, they cannot both be stored as separate entries with the same outer
`(DeviceId::PCI, 1, 0)` key. Canonical full-machine snapshots store PCI core state as a *single* `DeviceId::PCI` entry
(inner `PCIC`; see `PCI core state` below). Legacy snapshots may use split-out `PCI_CFG` + `PCI_INTX` entries.

#### ACPI PM (`DeviceId::ACPI_PM`)

Determinism note: the ACPI PM timer register `PM_TMR` must be derived from **deterministic platform time**
(`Clock::now_ns()` / a shared `ManualClock`), not host wall-clock time (e.g. `std::time::Instant`). Snapshot
restore reconstructs the timer phase relative to `now_ns()`, so using a non-deterministic time source will
cause the restored `PM_TMR` value/phase to differ across restores.

#### HPET (`DeviceId::HPET`)

Restore note: after applying HPET state, call `Hpet::poll(&mut sink)` (or run an equivalent one-shot platform
timer tick/poll) once to re-drive any asserted **level-triggered** GSIs implied by the restored
`general_int_status`. The per-timer `irq_asserted` field is runtime/sink-handshake state and is intentionally
not snapshotted, so a post-restore poll is required to make the sink observe restored asserted levels.

#### Snapshot device IDs (`DeviceId`) and web runtime `kind` strings

The snapshot file stores device entries under an **outer numeric** `DeviceId` (the `u32` written into each `DeviceState` header).

In the worker-based web runtime, device blobs are exchanged as `{ kind: string, bytes: Uint8Array }` (see `web/src/runtime/snapshot_protocol.ts`). The `kind` field is a **stable string** that maps to a numeric `DeviceId`.

For forward compatibility, the runtime also supports a fallback spelling for unknown device IDs:

- `device.<id>` (e.g. `device.1234`) → `DeviceId(<id>)`

| DeviceId enum | Numeric id | Web runtime `kind` | Notes |
|---|---:|---|---|
| `PIC` | `1` | `device.1` | Legacy PIC (guest-visible state) |
| `APIC` | `2` | `device.2` | Local APIC (guest-visible state) |
| `PIT` | `3` | `device.3` | PIT timer |
| `RTC` | `4` | `device.4` | RTC/CMOS |
| `PCI` | `5` | `device.5` | PCI config mechanism + bus state |
| `DISK_CONTROLLER` | `6` | `device.6` | Disk controller(s) + DMA state |
| `VGA` | `7` | `device.7` | VGA/VESA device model |
| `SERIAL` | `8` | `device.8` | UART 16550 |
| `CPU_INTERNAL` | `9` | `device.9` | Non-architectural CPU bookkeeping (pending IRQs, etc.) |
| `BIOS` | `10` | `device.10` | Firmware/BIOS runtime state |
| `MEMORY` | `11` | `device.11` | Memory-bus glue state (A20, ROM windows, etc.) |
| `USB` | `12` | `usb.uhci` | Browser USB stack (UHCI controller + runtime/bridge state) |
| `I8042` | `13` | `device.13` | Legacy i8042 PS/2 controller state |
| `PCI_CFG` | `14` | `device.14` | Legacy split PCI config ports + bus state (prefer `DeviceId::PCI`) |
| `PCI_INTX` | `15` | `device.15` | Legacy split PCI INTx routing state (prefer `DeviceId::PCI`) |
| `ACPI_PM` | `16` | `device.16` | ACPI fixed-feature power management state |
| `HPET` | `17` | `device.17` | High Precision Event Timer state |
| `HDA` | `18` | `device.18` | HD Audio controller/runtime state |
| `E1000` | `19` | `net.e1000` | Intel E1000 NIC device model state |
| `NET_STACK` | `20` | `net.stack` | User-space network stack/backend state (DHCP/NAT/proxy bookkeeping) |
| `PLATFORM_INTERRUPTS` | `21` | `device.21` | Platform interrupt controller/routing state |

#### USB (`DeviceId::USB`)

For the browser USB stack (guest-visible UHCI controller + runtime/bridge state), store a single device entry:

- Outer `DeviceState.id = DeviceId::USB`
- `DeviceState.data = aero-io-snapshot` TLV blob produced by the USB stack (inner `DEVICE_ID` examples: `UHRT` for `UhciRuntime`, `UHCB` for `UhciControllerBridge`, `WUHB` for `WebUsbUhciBridge`)
- `DeviceState.version` / `DeviceState.flags` mirror the inner device `SnapshotVersion (major, minor)` per the `aero_snapshot::io_snapshot_bridge` convention

Restore note: USB snapshots capture guest-visible controller/runtime state only. Any host-side "action" state (e.g. in-flight WebUSB/WebHID requests) should be treated as reset on restore; the host integration is responsible for resuming action execution post-restore.

This standardizes device payloads on a deterministic, forward-compatible TLV encoding. `aero-snapshot` provides opt-in helpers behind the `io-snapshot` feature: `aero_snapshot::io_snapshot_bridge::{device_state_from_io_snapshot, apply_io_snapshot_to_device}`.

#### i8042 (`DeviceId::I8042`)

For the legacy PS/2 controller (`ports 0x60/0x64`), store a single device entry:

- Outer `DeviceState.id = DeviceId::I8042`
- `DeviceState.data = aero-io-snapshot` TLV blob produced by the i8042 controller
  (`aero_devices_input::I8042Controller::save_state()`)
- `DeviceState.version` / `DeviceState.flags` mirror the inner device `SnapshotVersion (major, minor)`

Restore note: the i8042 snapshot includes the guest-visible output-port A20 bit (command `0xD0`).
If the i8042 model has a `SystemControlSink` attached (recommended), `load_state()` will
resynchronize the platform A20 latch with the restored output-port image.

IRQ note: i8042 IRQ1/IRQ12 are treated as **edge-triggered** in Aero’s model. The i8042 device
snapshot should not attempt to encode an “IRQ level”; any pending interrupts from prior edges must
be captured/restored by the interrupt controller (PIC/APIC) device state. On restore, the i8042
device must avoid emitting spurious IRQ pulses purely due to buffered output bytes (see
[`docs/irq-semantics.md`](./irq-semantics.md)).

#### Networking (`DeviceId::E1000`, `DeviceId::NET_STACK`)

Networking snapshots in Aero are split into two `DEVICES` entries:

1. **NIC device model state** (guest-visible hardware)
2. **Network backend/stack state** (guest-visible “host-side” network behavior)

Canonical identifiers (outer `DeviceId` + web/WASM `kind` string):

| Layer | Outer `DeviceId` | Numeric id | `DeviceId::name()` | Recommended `kind` | Notes |
|---|---|---:|---|---|---|
| NIC (E1000) | `DeviceId::E1000` | `19` | `"E1000"` | `net.e1000` | Registers + descriptor ring state + pending frames. Payload is an `aero-io-snapshot` blob (inner `DEVICE_ID = E1K0`). |
| Net stack/backend | `DeviceId::NET_STACK` | `20` | `"NET_STACK"` | `net.stack` | User-space network stack / NAT / DHCP / connection bookkeeping. Payload is an `aero-io-snapshot` blob. |

Payload convention:

- `DeviceState.data` stores the raw `aero-io-snapshot` TLV blob returned by `IoSnapshot::save_state()` (including the inner 16-byte `AERO` header).
- `DeviceState.version` / `DeviceState.flags` mirror the **inner device** `SnapshotVersion (major, minor)` from the contained `aero-io-snapshot` header.

Restore semantics (what is *not* bit-restorable):

- **Active TCP proxy connections** (e.g. WebSocket `/tcp` or `/tcp-mux` streams) are host resources and cannot be snapshotted/restored bit-for-bit.
- **L2 tunnel transports** (WebSocket `/l2` sessions, WebRTC DataChannels) are host resources and cannot be snapshotted/restored bit-for-bit.

On restore, integrations should treat these host resources as reset: immediately drop/close any live proxy/tunnel connections and re-establish them (or leave them disconnected and let the guest reconnect) after the VM resumes.

### PCI core state (`aero-devices`)

The PCI core in `crates/devices` snapshots only **guest-visible PCI-layer state** (not device-internal
emulation state) using `aero-io-snapshot` TLVs.

**Snapshot layout constraint:** `aero_snapshot` rejects duplicate `(DeviceId, version, flags)` tuples in `DEVICES` (see above). Since
`DeviceState.version/flags` mirror the inner `aero-io-snapshot` device `(major, minor)`, `PciConfigPorts` (`PCPT`) and `PciIntxRouter`
(`INTX`) cannot both be stored as separate `(DeviceId::PCI, 1, 0)` entries.

Therefore, **PCI core state must be stored as a single `DeviceId::PCI` entry** whose payload is an `aero-io-snapshot` blob produced by
the PCI core wrapper (`aero_devices::pci::PciCoreSnapshot`, inner `DEVICE_ID = PCIC`).

That `PCIC` wrapper contains nested io-snapshots as TLV fields:

- tag `1`: `PCPT` — `PciConfigPorts`: wraps the config mechanism state and per-function config spaces:
  - `PCF1` — `PciConfigMechanism1` 0xCF8 address latch
  - `PCIB` — `PciBusSnapshot` (per-BDF 256-byte config space image + BAR base/probe state)
- tag `2`: `INTX` — `PciIntxRouter` (PIRQ routing + asserted INTx levels)

Backward compatibility note: older snapshots may store these as split entries (`PCI_CFG` + `PCI_INTX`), or store `PCPT` under the legacy
outer id `DeviceId::PCI`. Snapshot restore code should accept those encodings, but snapshot writers should prefer the single-entry `PCI`
wrapper above to match the canonical layout.

Restore ordering note: `PciIntxRouter::load_state()` restores internal refcounts but cannot touch the platform
interrupt sink. Snapshot restore code should call `PciIntxRouter::sync_levels_to_sink()` **after restoring the
platform interrupt controller complex** (typically the `DeviceId::PLATFORM_INTERRUPTS` snapshot in Aero’s machine
model, or the historical `DeviceId::APIC` id in older snapshots) to re-drive any asserted GSIs (e.g.
level-triggered INTx
lines).

### Storage controllers (AHCI / IDE / ATAPI)

Storage controller device models should snapshot **guest-visible controller state** (registers, in-flight PIO/DMA progress, IRQ status) but must not inline host-specific disk/ISO backends or any disk contents.

In the `crates/aero-devices-storage` stack this means:

- AHCI (`AhciController` / `AhciPciDevice`) snapshots capture HBA + per-port register state (inner `aero-io-snapshot` `DEVICE_ID = AHCI` / `AHCP`).
- PIIX3 IDE (`Piix3IdePciDevice`) snapshots capture PCI config, channel taskfile/status, and Bus Master IDE state (inner `DEVICE_ID = IDE0`).

**Restore contract:** storage controllers intentionally drop any attached host backends during `IoSnapshot::load_state()`:

- AHCI ports clear attached `AtaDrive` instances.
- IDE ATA/ATAPI channels drop attached disks/ISOs; ATAPI devices are restored as “placeholder” CD-ROMs so guest-visible sense/tray/media flags survive without a backend.

The platform/coordinator must **re-attach** the appropriate disks/ISOs after restore (based on external configuration) before resuming guest execution.

For deterministic guest bring-up, reattachment should follow the same canonical topology/attachment mapping used at initial boot; for Windows 7 this is documented in [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md).

ATAPI note: the guest-visible “disc present” state is snapshotted explicitly (independent of whether a host ISO backend is currently attached), so reattaching an ISO backend does not implicitly “insert media” from the guest’s perspective.

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
- **Optional v2 extension** (added after the initial v2 release; older v2 snapshots may end at the FXSAVE bytes):
  - `u32 EXT_LEN` (number of extension bytes that follow; currently `4`)
  - `u8 a20_enabled`
  - `u8 irq13_pending`
  - `u8 pending_bios_int_valid`
  - `u8 pending_bios_int`

Note: the CPU snapshot is intended to match the canonical `aero_cpu_core::state::CpuState` shape
(architectural state plus a small amount of core execution bookkeeping like A20 gate state and BIOS
interrupt hypercall tracking). Higher-level runtime bookkeeping that lives outside `CpuState` (e.g.
pending external interrupts / interrupt-shadow state / local APIC queues) should be captured via the
`DEVICES` section by the snapshot adapter (see `CPU_INTERNAL` below).

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

Note: the execution core also tracks virtual time/TSC progression in non-ABI runtime state
(`aero_cpu_core::time::TimeSource`, typically `CpuCore.time`). Snapshot restore code should
re-seed that time source from `TSC` (and keep `CpuState.msr.tsc` coherent) when restoring a VM.
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
  - To enforce this at restore time, use `restore_snapshot_with_options()` and pass
    `RestoreOptions { expected_parent_snapshot_id: <currently loaded snapshot id> }`.
  - If you have a seekable reader (`Read + Seek`), use `aero_snapshot::restore_snapshot_with_options`
    and pass `RestoreOptions { expected_parent_snapshot_id: Some(base_snapshot_id) }` to guard
    against accidentally applying a diff to the wrong base.
  - If you only have a non-seekable reader (`Read`, e.g. a network stream), use
    `aero_snapshot::restore_snapshot_checked` with the same `RestoreOptions`.
    - **Ordering requirement:** for non-seekable restores, dirty snapshots must place the `META`
      section *before* the `RAM` section so the parent id can be validated safely before diffs are
      applied.
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

### Streaming snapshots (multi-GB)

For Windows 7–scale guests, snapshot files can be multiple gigabytes. Avoid building a giant `Vec<u8>` in either Rust or JS:

- `aero_snapshot::save_snapshot` is streaming-friendly but requires a `std::io::Write + Seek` target (it seeks back to patch section lengths).
- In Dedicated Workers, Chrome exposes OPFS `FileSystemSyncAccessHandle`, which supports positioned `read/write({ at })` operations.
- `crates/aero-opfs` provides `aero_opfs::OpfsSyncFile`, a `std::io::{Read, Write, Seek}` wrapper over `FileSystemSyncAccessHandle` with a cursor and JS-safe offset validation.

The browser/WASM bindings (`crates/aero-wasm`) expose convenience helpers (wasm32-only) for
streaming snapshots directly to/from OPFS:

- `snapshot_full_to_opfs(path: string) -> Promise<void>`
- `snapshot_dirty_to_opfs(path: string) -> Promise<void>`
- `restore_snapshot_from_opfs(path: string) -> Promise<void>`

These helpers are implemented for:

- `crates/aero-wasm::Machine` (preferred; canonical full-system VM)
- `crates/aero-wasm::DemoVm` (legacy stub/demo VM used by snapshot panels)

If sync access handles are unavailable (e.g. the code runs on the main thread instead of a DedicatedWorkerGlobalScope), these helpers return a clear error.

---

## Snapshot ordering requirements (multi-worker web runtime)

The worker-based web runtime splits responsibilities across workers (CPU, I/O, net). To produce a coherent snapshot and avoid replaying stale network traffic after restore, snapshot orchestration must follow a strict ordering:

1. **Pause CPU + I/O workers** so:
   - guest execution stops, and
   - device emulation stops mutating device/RAM state while it is being serialized.
2. **Drain/reset the net worker** *after* CPU+I/O are paused:
   - host tunnel connections (WebSocket/WebRTC) are **not bit-restorable**, so they must be treated as reset on restore.
   - clear the shared `NET_TX` / `NET_RX` rings so any in-flight frames are dropped rather than replayed into a restored guest.
3. Save/restore the snapshot payload (CPU/MMU + device blobs + guest RAM).
4. Resume workers; the net worker reconnects best-effort.

Networking-specific restore semantics (what is and is not bit-restorable) are documented in `docs/07-networking.md`.

---

## UI expectations

The UI should expose:

- **Save** (manual snapshot)
- **Load** (restore snapshot)
- **Auto-save interval** (e.g., every N seconds; disabled when set to 0)

The JS host can store the snapshot in OPFS and/or trigger a download for export.

---

## Testing

The canonical machine (`crates/aero-machine/`) contains deterministic host-side integration tests
that exercise snapshot round-trips end-to-end:

- `crates/aero-machine/tests/bios_post_checkpoint.rs`

The legacy stub VM (`crates/legacy/aero-vm/`) also contains deterministic tests (kept for historical reference):

- Run a deterministic program, snapshot mid-execution, restore into a fresh VM, and verify identical output + memory.
- Chain `full snapshot -> dirty diff snapshot` to validate incremental restore.

`crates/aero-snapshot/` additionally includes a `proptest`-based decoder robustness test that feeds random byte strings into the decoder and asserts it does not panic.
