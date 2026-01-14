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
  - `disk_id` is a **stable virtual disk slot identifier** used by host integrations to re-open
    disk backends after restore (it is not an OS-visible enumerator).
  - Canonical Windows 7 storage topology mapping (see [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md)):
    - `disk_id = 0` → AHCI ICH9 port 0 HDD
    - `disk_id = 1` → IDE PIIX3 secondary master ATAPI CD-ROM
    - `disk_id = 2` → (optional) IDE PIIX3 primary master ATA disk
- `CPUS` entries are written in canonical order: ascending `apic_id`.
- `MMUS` entries are written in canonical order: ascending `apic_id`.
- Dirty-page RAM snapshots canonicalize the dirty page list: sorted ascending, deduplicated, and validated against the guest RAM size.
- Snapshot restore also canonicalizes list ordering **before** passing data to the restore target:
  - `restore_snapshot` sorts `CPUS` by `apic_id`, `MMUS` by `apic_id`, `DEVICES` by `(device_id, version, flags)`, and `DISKS` by `disk_id`.
  - This makes restore deterministic even if a snapshot producer writes entries in arbitrary order.

### Validation / corruption handling

To keep restore behavior deterministic and avoid ambiguous state merges, the decoder treats the following as **corrupt**:

- Duplicate core sections (e.g. multiple `META`/`MMU`/`MMUS`/`DEVICES`/`DISKS`/`RAM` sections).
- Multiple CPU sections (any mix of `CPU` + `CPUS`).
- Ambiguous MMU state (any mix of `MMU` + `MMUS`).
- Duplicate entries inside canonical lists:
  - `DEVICES`: duplicate `(device_id, version, flags)` (must be unique)
  - `DISKS`: duplicate `disk_id` (must be unique)
  - `CPUS`: duplicate `apic_id` (must be unique)
  - `MMUS`: duplicate `apic_id` (must be unique)
- Dirty RAM snapshots (`RAM` `mode = Dirty`) whose page index list is not **strictly increasing**.

### Format limits / resource bounds

To keep snapshot parsing bounded (and avoid accidentally allocating unbounded memory when restoring untrusted/corrupt files), `aero-snapshot` enforces a set of **hard limits** at restore-time.

The canonical set of limits lives in `crates/aero-snapshot/src/limits.rs` (`aero_snapshot::limits`) and is shared by:

- `aero_snapshot::{save_snapshot,restore_snapshot}` (the library)
- `cargo xtask snapshot validate` (the tooling)

Current notable limits include:

- Max CPUs: `256`
- Max device entries: `4096`
- Max DEVICES section payload: `256 MiB`
- Max single device entry payload: `64 MiB`
- Max vCPU internal state blob: `64 MiB`
- Max disk overlay refs: `256`
- Max disk path length: `64 KiB`
- Max META label length: `4 KiB`
- Max RAM page size: `2 MiB`
- Max RAM chunk size: `64 MiB`

`save_snapshot` also enforces these bounds so Aero does not produce snapshots it cannot restore itself.

### Core sections (format v1)

| Section | Contents |
|--------:|----------|
| `META` | Snapshot id, parent id, timestamp, optional label |
| `CPU` | Architectural CPU state (v1: minimal; v2: `aero_cpu_core::state::CpuState` compatible) |
| `CPUS` | Multi-vCPU CPU state (v1: minimal; v2: `aero_cpu_core::state::CpuState` compatible) |
| `MMU` | **Legacy single-vCPU** MMU state (`MmuState`) (v1: minimal; v2: control/debug/MSR + descriptor tables). SMP snapshots use `MMUS`. |
| `MMUS` | Multi-vCPU MMU state (list of per-vCPU `MmuState` keyed by `apic_id`) |
| `DEVICES` | List of device states (PIC/APIC/PLATFORM_INTERRUPTS/PIT/RTC/I8042/HPET/ACPI_PM/PCI_CFG/PCI_INTX_ROUTER/DISK_CONTROLLER/VGA/etc) as typed TLVs |
| `DISKS` | References to disk base images + overlay images |
| `RAM` | Guest RAM contents (either full snapshot or dirty-page diff) |

Section IDs are numeric (`u32`) and are defined by `aero_snapshot::SectionId`:
`META=1`, `CPU=2`, `MMU=3`, `DEVICES=4`, `DISKS=5`, `RAM=6`, `CPUS=7`, `MMUS=8`.

### Recommended device payload convention (DEVICES)

`aero-snapshot` stores device entries as opaque `DeviceState { id, version, flags, data }`. For new device models, the recommended convention is:

- `DeviceState.id`: the *outer* Aero `DeviceId` (assigned by the coordinator)
- `DeviceState.data`: the raw `aero-io-snapshot` TLV blob returned by `IoSnapshot::save_state()` (including the inner header)
- `DeviceState.version` / `DeviceState.flags`: mirror the inner *device* `SnapshotVersion` from the `aero-io-snapshot` header (`version = device_major`, `flags = device_minor`, not the io-snapshot format version)

#### PcPlatform (full-system) device coverage

A minimal VM can sometimes get away with snapshotting just CPU/CPUS + MMU/MMUS + BIOS + RAM, but a full PcPlatform-based machine must also snapshot the platform devices that define interrupt/timer routing and input state.

Beyond BIOS/RAM, a PcPlatform snapshot is expected to include `DEVICES` entries for:

- **Platform interrupt controller complex** (legacy PIC + IOAPIC + LAPIC + IMCR routing; typically stored as `DeviceId::PLATFORM_INTERRUPTS`, historically `DeviceId::APIC`)
- **Timers:** PIT, RTC, and HPET
- **ACPI PM fixed-function I/O** (SCI + PM_TMR, plus PM1/GPE state)
- **i8042 controller** (PS/2 keyboard/mouse controller and output-port state)

Restore notes:

- **HPET:** after restoring HPET state *and* the interrupt controller, call `Hpet::sync_levels_to_sink(&mut sink)` to re-drive any pending **level-triggered** interrupt lines based on `general_int_status`. (`load_state()` intentionally does not directly touch the interrupt sink.) As a fallback, `Hpet::poll(&mut sink)` also reasserts levels, but it advances HPET time and is therefore less ideal for deterministic restore.
- **ACPI PM determinism:** `PM_TMR` must be derived from deterministic virtual time for stable snapshot/restore. Avoid basing it on host wall-clock / `Instant` (which will make snapshots nondeterministic and can introduce time jumps on restore).

#### Common platform device ids (outer `DeviceId`)

Some platform devices are snapshotted as their own `DEVICES` entries and use dedicated `DeviceId` constants:

- `DeviceId::APIC` (`2`) — platform interrupt controller complex (`PlatformInterrupts`: legacy PIC + IOAPIC + LAPIC + IMCR routing). Historical id used by older snapshots (inner `INTR`).
- `DeviceId::PLATFORM_INTERRUPTS` (`21`) — preferred id for `PlatformInterrupts` snapshots (used by the canonical machine) (inner `INTR`)
- `DeviceId::PIT` (`3`) — PIT (8254; inner `PIT4`)
- `DeviceId::RTC` (`4`) — RTC/CMOS (0x70/0x71; inner `RTCC`)
- `DeviceId::PCI_CFG` (`14`) — PCI config I/O ports + PCI bus config-space image state (`PciConfigPorts`, inner `PCPT`)
- `DeviceId::PCI_INTX_ROUTER` (`15`) — PCI INTx router (`PciIntxRouter`, inner `INTX`)
- `DeviceId::PCI` (`5`) — legacy PCI core state (either a `PciCoreSnapshot` wrapper, inner `PCIC`, or a historical `PciConfigPorts` snapshot, inner `PCPT`, or a historical `PciIntxRouter` snapshot, inner `INTX`; see `PCI core state` below)
- `DeviceId::DISK_CONTROLLER` (`6`) — Storage controller(s) (single `DSKC` wrapper containing per-BDF controller snapshots; see `Disk controllers` below)
- `DeviceId::I8042` (`13`) — i8042 PS/2 controller (0x60/0x64; inner `8042`)
- `DeviceId::ACPI_PM` (`16`) — ACPI power management registers (PM1 + PM timer; inner `ACPM`)
- `DeviceId::HPET` (`17`) — HPET timer state (inner `HPET`)
- `DeviceId::HDA` (`18`) — guest-visible HD Audio (HDA) controller/runtime state (inner `HDA0`)
- `DeviceId::E1000` (`19`) — Intel E1000 NIC (`aero-net-e1000`, inner `E1K0`)
- `DeviceId::NET_STACK` (`20`) — user-space network stack/backend state (`aero-io-snapshot` inner `NETS`; legacy NAT stack state uses `NETL`)
- `DeviceId::VIRTIO_SND` (`22`) — guest-visible virtio-snd (virtio-pci) device state (web runtime: inner `VSND`)
- `DeviceId::VIRTIO_NET` (`23`) — guest-visible virtio-net (virtio-pci) NIC transport state (inner `VPCI`)
- `DeviceId::VIRTIO_INPUT` (`24`) — guest-visible virtio-input (virtio-pci) multi-function device state (keyboard + mouse) (canonical machine: wrapper inner `VINP`)
- `DeviceId::AEROGPU` (`25`) — AeroGPU device state

Note: `aero-snapshot` rejects duplicate `(DeviceId, version, flags)` tuples inside `DEVICES`. Since both `PciConfigPorts` and
`PciIntxRouter` currently snapshot as `SnapshotVersion (1.0)`, they cannot both be stored as separate entries with the same outer
`(DeviceId::PCI, 1, 0)` key. Canonical full-machine snapshots store PCI core state using *separate* `DeviceId::PCI_CFG` +
`DeviceId::PCI_INTX_ROUTER` entries (see `PCI core state` below). Restore code should remain compatible with legacy snapshots that stored PCI
config ports under `DeviceId::PCI`, stored PCI INTx routing under `DeviceId::PCI`, or stored a combined `PciCoreSnapshot` wrapper (`PCIC`)
under `DeviceId::PCI`.

#### ACPI PM (`DeviceId::ACPI_PM`)

Determinism note: the ACPI PM timer register `PM_TMR` must be derived from **deterministic platform time**
(`Clock::now_ns()` / a shared `ManualClock`), not host wall-clock time (e.g. `std::time::Instant`). Snapshot
restore reconstructs the timer phase relative to `now_ns()`, so using a non-deterministic time source will
cause the restored `PM_TMR` value/phase to differ across restores.

#### HPET (`DeviceId::HPET`)

Restore note: after applying HPET state, call `Hpet::sync_levels_to_sink(&mut sink)` once to re-drive any
asserted **level-triggered** GSIs implied by the restored
`general_int_status`. The per-timer `irq_asserted` field is runtime/sink-handshake state and is intentionally
not snapshotted, so a post-restore sync is required to make the sink observe restored asserted levels.

#### Snapshot device IDs (`DeviceId`) and web runtime `kind` strings

The snapshot file stores device entries under an **outer numeric** `DeviceId` (the `u32` written into each `DeviceState` header).

In the worker-based web runtime, device blobs are exchanged as `{ kind: string, bytes: Uint8Array }` (protocol types in `web/src/runtime/snapshot_protocol.ts`). The `kind` field is a **stable string** that maps to a numeric `DeviceId` (mapping lives in `web/src/workers/vm_snapshot_wasm.ts` and is mirrored in `crates/aero-wasm/src/vm_snapshot_device_kind.rs` (used by the WASM-side snapshot builder)).

For forward compatibility, the runtime also supports a fallback spelling for unknown device IDs:

- `device.<id>` (e.g. `device.1234`) → `DeviceId(<id>)`
  - `<id>` is a **decimal `u32`** in the range `0..=4294967295`.
  - The wire grammar is intentionally restricted to **ASCII digits only** (`^[0-9]+$`; no `+`/`-`).
  - Parsers may accept leading zeros, but canonical encoders should emit `device.<id>` without leading zeros.

| DeviceId enum | Numeric id | Web runtime `kind` | Notes |
|---|---:|---|---|
| `PIC` | `1` | `device.1` | Legacy PIC (guest-visible state) |
| `APIC` | `2` | `device.2` | Legacy id for the platform interrupt controller complex (`PlatformInterrupts`, inner `INTR`); prefer `PLATFORM_INTERRUPTS` (`21`) |
| `PIT` | `3` | `device.3` | PIT timer |
| `RTC` | `4` | `device.4` | RTC/CMOS |
| `PCI` | `5` | `device.5` | Legacy PCI core state (old snapshots; prefer `PCI_CFG` + `PCI_INTX_ROUTER`) |
| `DISK_CONTROLLER` | `6` | `device.6` | Disk controller(s) + DMA state (single `DSKC` wrapper; see below) |
| `VGA` | `7` | `device.7` | VGA/VESA device model |
| `SERIAL` | `8` | `device.8` | UART 16550 |
| `CPU_INTERNAL` | `9` | `device.9` | Non-architectural CPU bookkeeping (pending IRQs, etc.) |
| `BIOS` | `10` | `device.10` | Firmware/BIOS runtime state |
| `MEMORY` | `11` | `device.11` | Memory-bus glue state (A20, ROM windows, etc.) |
| `USB` | `12` | `usb` | Browser USB stack (USB controller state; may contain UHCI/EHCI/xHCI controller blobs). Legacy/compat kind: `usb.uhci` |
| `I8042` | `13` | `input.i8042` | Legacy i8042 PS/2 controller state |
| `PCI_CFG` | `14` | `device.14` | PCI config ports + bus state (Rust: `PciConfigPorts`, inner `PCPT`; web: JS PCI bus snapshot, inner `PCIB`) |
| `PCI_INTX_ROUTER` | `15` | `device.15` | PCI INTx routing state (`PciIntxRouter`, inner `INTX`) |
| `ACPI_PM` | `16` | `device.16` | ACPI fixed-feature power management state |
| `HPET` | `17` | `device.17` | High Precision Event Timer state |
| `HDA` | `18` | `audio.hda` | HD Audio controller/runtime state |
| `E1000` | `19` | `net.e1000` | Intel E1000 NIC device model state |
| `NET_STACK` | `20` | `net.stack` | User-space network stack/backend state (DHCP/DNS cache + connection bookkeeping) |
| `PLATFORM_INTERRUPTS` | `21` | `device.21` | Platform interrupt controller/routing state |
| `VIRTIO_SND` | `22` | `audio.virtio_snd` | virtio-snd (virtio-pci) audio device state |
| `VIRTIO_NET` | `23` | `net.virtio_net` | virtio-net (virtio-pci) NIC transport state |
| `VIRTIO_INPUT` | `24` | `input.virtio_input` | virtio-input (virtio-pci) multi-function device state (keyboard + mouse) |
| `AEROGPU` | `25` | `gpu.aerogpu` | AeroGPU device state |
| `GPU_VRAM` | `28` | `gpu.vram` | Web runtime GPU VRAM/BAR1 backing store (guest-visible scanout memory). May be chunked across multiple `(DeviceId, version, flags)` entries. On restore, the IO worker applies VRAM bytes locally and does **not** forward them to the coordinator. |

Note: older runtimes that do not recognize `gpu.aerogpu` may still encode/decode this device as
`device.25` (the generic fallback spelling). This is acceptable for forward compatibility.

#### GPU VRAM (`DeviceId::GPU_VRAM` = `28`, kind `gpu.vram`)

In the web runtime, GPU VRAM (the SharedArrayBuffer-backed BAR1 mapping) is stored as one or more
`gpu.vram` device blobs.

- Chunking: VRAM may be split across multiple outer `DeviceId::GPU_VRAM` entries.
  - Each chunk uses a distinct `DeviceState.flags` value (`flags = chunk_index`) so the snapshot file
    does not contain duplicate `(DeviceId, version, flags)` keys.
- Payload format: each chunk payload starts with a legacy 8-byte `"AERO"` header
  (`version = 1`, `flags = chunk_index`) followed by per-chunk metadata (total length, offset, chunk
  length) and then raw VRAM bytes.
- Restore policy: when the IO worker is given a VRAM buffer (`vramU8`), it applies `gpu.vram` blobs
  directly into that buffer and does **not** forward them to the coordinator (to avoid transferring
  large blobs through `postMessage`).

The web runtime may also store **host-managed** snapshot state under reserved `device.<id>` entries that do **not** correspond to a Rust `DeviceId` enum variant. These blobs are treated as opaque by the Rust snapshot tooling and exist to make browser-specific integrations round-trip correctly across save/restore.

Current reserved web-only ids:

- `device.1000000000` — `RuntimeDiskWorker` snapshot state (disk overlay refs + remote cache bindings). This is host-side persistence state and is separate from the guest-visible disk controller snapshot (`DeviceId::DISK_CONTROLLER`).
- `device.1000000001+` — **legacy/compat** IO worker BAR1 VRAM snapshot chunks (kept for restore compatibility).
  - Older web builds stored the IO worker's SharedArrayBuffer-backed BAR1 VRAM mapping as raw chunk bytes under reserved `device.<id>` entries.
  - Chunk `i` was stored as `device.${1000000001 + i}` and implicitly addressed as `start = i * chunkBytes` (`chunkBytes <= 64 MiB`).
  - Each chunk is **≤ 64 MiB** (`aero_snapshot::limits::MAX_DEVICE_ENTRY_LEN`), so the current 128 MiB web VRAM configuration uses up to 2 chunks.
  - Newer builds snapshot BAR1 VRAM via the canonical `gpu.vram` (`DeviceId::GPU_VRAM = 28`) device blobs, with explicit offset/length metadata embedded in each payload (no chunk size hint required).
  - On restore, the IO worker applies these blobs directly into its BAR1 view and does **not** forward them to the coordinator (to avoid transferring 64–128 MiB through `postMessage`).

#### Disk controllers (`DeviceId::DISK_CONTROLLER` = `6`)

`aero-snapshot` rejects duplicate `(DeviceId, version, flags)` tuples inside `DEVICES` (see `Validation / corruption handling` above), and by convention `DeviceState.version/flags` mirror the *inner* `aero-io-snapshot` device `SnapshotVersion (major, minor)` (see `Recommended device payload convention (DEVICES)` above).

This means multiple storage controllers **cannot** be stored as multiple outer `DeviceId::DISK_CONTROLLER` entries if they share the same inner `SnapshotVersion` (`major`, `minor`).

Historically, several controller snapshots started at `SnapshotVersion (1.0)` (for example, AHCI `AHCP` and early virtio-pci `VPCI`), which would collide as `(DeviceId::DISK_CONTROLLER = 6, version = 1, flags = 0)` and be treated as corrupt.

Note: `VPCI` is currently `SnapshotVersion (1.2)`, but the `DSKC` wrapper remains the canonical encoding because it allows an arbitrary set of controllers (including future ones that may share snapshot versions) to coexist under a single outer `DeviceId::DISK_CONTROLLER` entry.

**Canonical encoding:** store **exactly one** outer `DeviceId::DISK_CONTROLLER` entry whose payload is an `aero-io-snapshot` TLV blob with inner `DEVICE_ID = DSKC` and `SnapshotVersion (1.0)`.

That `DSKC` wrapper can then contain *multiple* nested controller snapshots keyed by a packed PCI BDF (`u16`) using the standard PCI config-address layout:

```text
packed_bdf = (bus << 8) | (device << 3) | function
```

Producers/consumers should prefer `aero_devices::pci::PciBdf::{pack_u16,unpack_u16}` (rather than re-encoding manually) to avoid inconsistencies.

Example nested controller payloads include:

- `AHCP` (AHCI PCI)
- `VPCI` (virtio-pci based storage controllers, e.g. virtio-blk)
- `NVMP` (NVMe PCI)

For deterministic encoding, nested controller entries should be written in canonical order: ascending PCI BDF.

Restore behavior: when restoring the `DSKC` wrapper, snapshot consumers should apply only the controller entries that exist in the target machine. Unknown/extra controller entries (unknown device id, unknown BDF, or simply a controller type that is not present) should be ignored for forward compatibility.

#### USB (`DeviceId::USB`)

For the browser USB stack (guest-visible USB controller(s) + runtime/bridge state), store a single device entry:

Snapshot policy (multiple controllers):

- The `DEVICES` list rejects duplicate `(DeviceId, version, flags)` keys, and the web runtime maps all
  guest USB controllers to the single outer `DeviceId::USB` slot.
- Therefore, the snapshot producer must emit **at most one** `DeviceId::USB` entry (or none if no
  USB state exists).
- When multiple controllers are present (UHCI + optional EHCI/xHCI), encode their per-controller
  snapshots under that single outer entry using an `"AUSB"` container
  (`web/src/workers/usb_snapshot_container.ts`):
  - Container entry tags (FourCC): `USB_SNAPSHOT_TAG_UHCI`, `USB_SNAPSHOT_TAG_EHCI`,
    `USB_SNAPSHOT_TAG_XHCI`
  - **Determinism:** entries are encoded in a deterministic order (sorted by tag).
  - **Backward-compat save policy:** if **only UHCI** state exists, emit a **plain UHCI blob** (no
    container) so older builds can still restore it.
  - **Restore policy:**
    - If the blob is an `"AUSB"` container, restore each contained controller blob into its matching
      runtime/bridge if present.
    - If a controller blob is present but the runtime lacks that controller, it is ignored with a
      warning (forward compatibility).
    - If the blob begins with `"AUSB"` magic but fails to decode as a container, it is treated as
      corrupt and ignored.

- Outer `DeviceState.id = DeviceId::USB`
- `DeviceState.data`:
  - Legacy: raw `aero-io-snapshot` TLV blob produced by a single USB controller snapshot (most
    commonly UHCI; some older builds stored raw xHCI/EHCI snapshots here before the AUSB container
    was introduced).
  - Multi-controller: `"AUSB"` container holding per-controller blobs (each typically an
    `aero-io-snapshot` TLV).
    - Inner `DEVICE_ID` examples:
      - UHCI: `UHRT` for `UhciRuntime`, `UHCB` for `UhciControllerBridge`, `WUHB` for `WebUsbUhciBridge`.
      - xHCI: `XHCI` for the core controller (`aero_usb::xhci::XhciController`), `XHCB` for the WASM
        bridge (`aero_wasm::XhciControllerBridge`), and `XHCP` for the native PCI wrapper
        (`aero_devices::usb::xhci::XhciPciDevice`).
    - `aero_machine::Machine` snapshots may store `DeviceId::USB` as a small adapter-level wrapper TLV (`USBC`) that nests one or more controller blobs (UHCI + optional EHCI + optional xHCI) plus per-controller host-managed timing accumulator state used for deterministic 1ms ticking.
  - `DeviceState.version` / `DeviceState.flags` mirror the inner device `SnapshotVersion (major, minor)` per the `aero_snapshot::io_snapshot_bridge` convention

Note (current browser runtime wiring):

- The outer web snapshot `kind` for `DeviceId::USB` is `usb` (legacy kind `usb.uhci` accepted; see
  `web/src/workers/vm_snapshot_wasm.ts`).
- The browser runtime stores **exactly one** outer `DeviceId::USB` device blob. When multiple USB
  controllers exist, per-controller snapshots are multiplexed under that single blob using an
  `"AUSB"` container (see policy above).

Restore note: USB snapshots capture guest-visible controller/runtime state only. Any host-side "action" state (e.g. in-flight WebUSB/WebHID requests) should be treated as reset on restore; the host integration is responsible for resuming action execution post-restore.

#### Networking stack (`DeviceId::NET_STACK`)

For the browser networking stack (e.g. tunnel/NAT state managed outside the guest-visible NIC device model), store a single device entry:

- Outer `DeviceState.id = DeviceId::NET_STACK`
- `DeviceState.data = aero-io-snapshot` TLV blob produced by the networking stack.
  - Canonical in-browser stack (`crates/aero-net-stack`): inner `DEVICE_ID = NETS` (legacy pre-canonical device id accepted)
  - Legacy NAT-based stack state (kept for backward compatibility): inner `DEVICE_ID = NETL`
- `DeviceState.version` / `DeviceState.flags` mirror the inner device `SnapshotVersion (major, minor)`

Restore note: `NET_STACK` snapshots capture *guest-visible* stack/backend bookkeeping (e.g. DHCP/DNS cache and connection bookkeeping) but **must not** attempt to bit-restore host resources. On restore, integrations should treat the following as reset and drop/recreate them:

- Active TCP proxy connections (e.g. WebSocket `/tcp` / `/tcp-mux`)
- L2 tunnel transports (WebSocket `/l2` (legacy alias: `/eth`), WebRTC `l2` DataChannels)

#### E1000 (`DeviceId::E1000`)

For the guest-visible Intel E1000 NIC emulation state (`crates/aero-net-e1000`), store a single device entry:

- Outer `DeviceState.id = DeviceId::E1000`
- `DeviceState.data = aero-io-snapshot` TLV blob produced by the E1000 device model (inner `DEVICE_ID = E1K0`)
- `DeviceState.version` / `DeviceState.flags` mirror the inner device `SnapshotVersion (major, minor)`

The NIC snapshot captures enough guest-visible state to resume RX/TX DMA correctly, including:

- key registers (CTRL/STATUS/IMS/ICR/RCTL/TCTL, ring base/len/head/tail, etc.)
- MAC + EEPROM/PHY state
- in-flight TX aggregation/offload bookkeeping
- pending host-facing RX/TX queues (Ethernet frames)

Restore note: host-side network backends/tunnels are external state and are intentionally *not* part of the
snapshot format. Canonical snapshot restore flows should treat the backend lifecycle as out-of-band
(e.g. detach on restore, then re-attach as needed).

This standardizes device payloads on a deterministic, forward-compatible TLV encoding. `aero-snapshot` provides opt-in helpers behind the `io-snapshot` feature: `aero_snapshot::io_snapshot_bridge::{device_state_from_io_snapshot, apply_io_snapshot_to_device}`.

#### i8042 / PS/2 controller (`DeviceId::I8042`)

For the legacy PS/2 controller (`ports 0x60/0x64`), store a single device entry:

- Outer `DeviceState.id = DeviceId::I8042`
- `DeviceState.data = aero-io-snapshot` TLV blob produced by the i8042 controller
  (`aero_devices_input::I8042Controller::save_state()`, inner `DEVICE_ID = 8042`)
- `DeviceState.version` / `DeviceState.flags` mirror the inner device `SnapshotVersion (major, minor)`

This captures controller registers, output buffers, and both PS/2 device states (keyboard + mouse), including the output-port A20 bit.

Restore note: the i8042 snapshot includes the guest-visible output-port A20 bit (command `0xD0`).
If the i8042 model has a `SystemControlSink` attached (recommended), `load_state()` will
resynchronize the platform A20 latch with the restored output-port image.

IRQ note: i8042 IRQ1/IRQ12 are treated as **edge-triggered** in Aero’s model. The i8042 device
snapshot should not attempt to encode an “IRQ level”; any pending interrupts from prior edges must
be captured/restored by the interrupt controller (PIC/APIC/PLATFORM_INTERRUPTS) device state. On restore, the i8042
device must avoid emitting spurious IRQ pulses purely due to buffered output bytes (see
[`docs/irq-semantics.md`](./irq-semantics.md)).

#### HDA audio (`DeviceId::HDA`)

For the guest-visible Intel HD Audio device model (HDA controller + codec + stream/DMA state), store a single device entry:

- Outer `DeviceState.id = DeviceId::HDA`
- `DeviceState.data = aero-io-snapshot` TLV blob produced by the HDA stack (inner `DEVICE_ID = HDA0`)
- `DeviceState.version` / `DeviceState.flags` mirror the inner device `SnapshotVersion (major, minor)`

Restore notes (see also [`docs/06-audio-subsystem.md`](./06-audio-subsystem.md#snapshotrestore-save-states)):

- The browser audio graph (`AudioContext` / `AudioWorkletNode`) is a host resource and is not captured in snapshots. On restore, host code must recreate/reattach the Web Audio pipeline.
- Audio snapshots preserve **ring buffer indices** (guest-visible progress) but do not restore ring **contents**. The producer should clear the ring to silence on restore to avoid replaying stale samples.

### PCI core state (`aero-devices`)

The PCI core in `crates/aero-devices` snapshots only **guest-visible PCI-layer state** (not device-internal emulation state) using `aero-io-snapshot` TLVs with the following inner `DEVICE_ID`s:

- `PCPT` — `PciConfigPorts`: wraps the config mechanism state and per-function config spaces:
  - `PCF1` — `PciConfigMechanism1` 0xCF8 address latch
  - `PCIB` — `PciBusSnapshot` (per-BDF 256-byte config space image + BAR base/probe state)
- `INTX` — `PciIntxRouter` (PIRQ routing + asserted INTx levels)

**Snapshot layout constraint:** `aero_snapshot` rejects duplicate `(DeviceId, version, flags)` tuples in `DEVICES` (see above). Since
`DeviceState.version/flags` mirror the inner `aero-io-snapshot` device `(major, minor)`, `PciConfigPorts` (`PCPT`) and `PciIntxRouter`
(`INTX`) cannot both be stored as separate `(DeviceId::PCI, 1, 0)` entries.

When storing these `aero-io-snapshot` blobs as **separate** `DEVICES` entries (rather than wrapping them in `PciCoreSnapshot`), they must
use **distinct outer** `DeviceId`s to avoid collisions:

- Preferred split layout:
  - `PciConfigPorts` (`PCPT`): store as outer `DeviceId::PCI_CFG` (`14`)
  - `PciIntxRouter` (`INTX`): store as outer `DeviceId::PCI_INTX_ROUTER` (`15`)
- Legacy/compat layouts (restore should accept):
  - `PciConfigPorts` (`PCPT`) stored under the historical outer `DeviceId::PCI` (`5`)
  - `PciIntxRouter` (`INTX`) stored under the historical outer `DeviceId::PCI` (`5`)

This split avoids `DEVICES` duplicate key collisions, since the section rejects duplicate `(device_id, version, flags)` tuples and both PCI
TLVs commonly share the same inner snapshot `(major, minor)` (e.g. v1.0). It also reflects different restore orchestration:
`PciIntxRouter` requires a post-restore `sync_levels_to_sink()` call once the platform interrupt controller/sink is restored, while the PCI
config ports snapshot can be restored earlier.

Backward compatibility:

- Some snapshot integrations store PCI core state as a single `DeviceId::PCI` (`5`) entry using a wrapper TLV (`aero_devices::pci::PciCoreSnapshot`, inner `DEVICE_ID = PCIC`) that nests both `PCPT` and `INTX` as fields.
- Older snapshots may store `PciConfigPorts` (`PCPT`) directly under `DeviceId::PCI` (`5`) instead of `DeviceId::PCI_CFG` (`14`).
- Older snapshots may store `PciIntxRouter` (`INTX`) directly under `DeviceId::PCI` (`5`) instead of `DeviceId::PCI_INTX_ROUTER` (`15`).

Restore code should accept these legacy layouts, but new snapshots should prefer the split-out `PCI_CFG` + `PCI_INTX_ROUTER` entries.
If both legacy `DeviceId::PCI` (`5`) entries and split-out `PCI_CFG`/`PCI_INTX_ROUTER` entries are present in the same snapshot, restore
implementations should prefer the split-out entries.

Restore ordering note: `PciIntxRouter::load_state()` restores internal refcounts but cannot touch the platform interrupt sink. Snapshot restore code should call `PciIntxRouter::sync_levels_to_sink()` **after restoring the platform interrupt controller complex** (typically the `DeviceId::PLATFORM_INTERRUPTS` snapshot in Aero’s machine model, or the historical `DeviceId::APIC` id in older snapshots) to re-drive any asserted GSIs (e.g. level-triggered INTx lines).

### Storage controllers (AHCI / IDE / ATAPI)

Storage controller device models should snapshot **guest-visible controller state** (registers, in-flight PIO/DMA progress, IRQ status) but must not inline host-specific disk/ISO backends or any disk contents.

In the `crates/aero-devices-storage` stack this means:

- AHCI (`AhciController` / `AhciPciDevice`) snapshots capture HBA + per-port register state (inner `aero-io-snapshot` `DEVICE_ID = AHCI` / `AHCP`).
- PIIX3 IDE (`Piix3IdePciDevice`) snapshots capture PCI config, channel taskfile/status, and Bus Master IDE state (inner `DEVICE_ID = IDE0`).

**Restore contract:** storage controllers intentionally drop any attached host backends during `IoSnapshot::load_state()`:

- AHCI ports clear attached `AtaDrive` instances.
- IDE ATA/ATAPI channels drop attached disks/ISOs; ATAPI devices are restored as “placeholder” CD-ROMs so guest-visible sense/tray/media flags survive without a backend.

The platform/coordinator must **re-attach** the appropriate disks/ISOs after restore (based on external configuration) before resuming guest execution.

#### `DISKS` section: `DiskOverlayRefs` + stable `disk_id`

The snapshot `DISKS` section encodes external disk image references as
`aero_snapshot::DiskOverlayRefs`:

- Each entry is a `DiskOverlayRef { disk_id, base_image, overlay_image }`.
- `base_image` / `overlay_image` are **opaque host identifiers** (e.g. a remote object key, a URL,
  or an OPFS path) and are not interpreted by `aero-snapshot`.
  - Empty strings are allowed and mean "not configured" for that field. Snapshot producers may
    still emit entries with empty fields to preserve a stable `disk_id` mapping even when no host
    backend is currently attached.

Note: `aero-snapshot` validates `base_image` / `overlay_image` only as bounded-length UTF-8 strings.
**Empty strings are allowed.** Some snapshot adapters (notably the canonical
`aero_machine::Machine`) intentionally emit placeholder entries for the canonical disk slots
(`disk_id = 0` and `disk_id = 1`) even when the host has not configured any disk/ISO backend yet; in
that case `base_image == ""` and `overlay_image == ""` should be interpreted as “no backend
configured” and ignored by restore code.

On restore, the snapshot adapter receives these refs via
`SnapshotTarget::restore_disk_overlays(...)`. Since storage controller `load_state()` drops host
backends, the host/platform must:

1. Use the `DiskOverlayRefs` entries to re-open the referenced base+overlay images (or other disk
   backends).
2. Use the stable `disk_id` mapping to attach each reopened backend to the correct controller/port
   before resuming guest execution.

For deterministic guest bring-up, reattachment must follow the same canonical
topology/attachment mapping used at initial boot; for Windows 7 this includes the stable `disk_id`
mapping documented in [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md).

If the topology or `disk_id` mapping changes, update that document and the corresponding guard
tests (notably `crates/aero-machine/tests/machine_disk_overlays_snapshot.rs`).

ATAPI note: the guest-visible “disc present” state is snapshotted explicitly (independent of whether a host ISO backend is currently attached), so reattaching an ISO backend does not implicitly “insert media” from the guest’s perspective.

Note: `Hpet::load_state()` restores `general_int_status` but cannot access the interrupt sink; snapshot restore code should call `Hpet::sync_levels_to_sink()` (or `Hpet::poll()`) after restoring the interrupt controller to re-drive any pending level-triggered timer interrupts.

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

The BIOS hypercall mechanism is based on the firmware installing IVT entries that point to ROM stubs
beginning with `HLT; IRET` (bytes `F4 CF`). When real/v8086 vector delivery enters one of these stubs,
Tier-0 treats the `HLT` as a `BiosInterrupt(vector)` exit rather than halting the CPU. The
`pending_bios_int_valid/pending_bios_int` fields capture that pending BIOS vector across snapshots.

---

## MMU section encoding

`MmuState` is **per-vCPU architectural state** (control/debug registers, descriptor tables, and key
MSRs like FS/GS base and TSC).

- `MMU` stores a single `MmuState` and is a **legacy / single-vCPU** section.
- SMP snapshots should use `MMUS`, which stores one `MmuState` per vCPU keyed by `apic_id`.

For both `MMU` and `MMUS`, the section TLV `section_version` selects the embedded `MmuState` encoding
version:

- `section_version = 1` — legacy minimal `MmuState`
- `section_version = 2` — current full `MmuState`

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

### MMUS section encoding

Payload layout:

```text
u32 count
repeat count times:
  u64 entry_len
  entry payload (entry_len bytes):
    u32 apic_id
    MmuState (encoded according to the MMUS section_version)
```

Determinism: `MMUS` entries are written in canonical order by ascending `apic_id` (and restore
re-sorts/canonicalizes by `apic_id` before passing entries to snapshot targets).

Backward compatibility:

- Older snapshots may include `CPUS` + a single `MMU` section. New restore implementations interpret
  that `MMU` payload as applying to **all** vCPUs in `CPUS`.

---

## CPU internal (device) encoding

Some CPU execution state is not part of the architectural `CPU`/`CPUS` + `MMU`/`MMUS` sections (e.g.
pending external interrupts). If needed, it is stored in the `DEVICES` section as a device entry:

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
See also the repo-wide disk/backend trait mapping in
[`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

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

1. **Pause CPU, then pause I/O, then pause NET** (via `vm.snapshot.pause`) so:
    - guest execution stops,
    - device emulation stops mutating device/RAM state while it is being serialized, and
    - the net worker stops the tunnel forwarder and drains any pending `NET_TX`/`NET_RX` traffic.
2. **Clear/reset `NET_TX` / `NET_RX`** once all ring participants are paused.
    - These shared-memory rings are **not part of the snapshot file**, and any in-flight frames must be dropped to avoid replay into a restored guest.
    - Host tunnel transports (WebSocket/WebRTC) are **not bit-restorable**, so restore must treat them as reset and reconnect best-effort.
3. Save/restore the snapshot payload (CPU/CPUS + MMU/MMUS + device blobs + guest RAM).
4. Resume workers (CPU+I/O first, then net); the net worker reconnects best-effort.

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
