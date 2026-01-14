# Canonical Storage Topology (Windows 7 Boot/Install)

This document defines the **canonical, deterministic storage device topology** used for Windows 7
boot and installation.

The goal is that **controller emulation**, **platform PCI wiring**, and **browser storage
backends** all agree on:

- which controllers exist,
- where they live on the PCI bus (**BDF**),
- how media is attached (AHCI **port** / IDE **channel+drive**),
- how interrupts are routed (PCI **INTx → GSI** under Aero’s `PciIntxRouterConfig`, plus legacy
  **IDE channel IRQ14/IRQ15** for ATA/ATAPI).

If you change this topology (BDFs, attachment points, or INTx routing), you must update this doc
and the corresponding tests:

- `crates/devices/tests/win7_storage_topology.rs` (PCI profile constants + INTx routing)
- `crates/aero-pc-platform/tests/pc_platform_win7_storage.rs` (platform integration wiring)
- `crates/aero-pc-platform/tests/windows7_storage_topology.rs` (end-to-end AHCI + ATAPI read tests)
- `crates/aero-machine/tests/machine_win7_storage_topology.rs` (canonical `Machine` wiring: BDFs + PCI Interrupt Line)
- `crates/aero-machine/tests/win7_storage_topology.rs` (guard: canonical storage BDFs + PCI Interrupt Line/Pin + IDE legacy BAR ranges + key BAR definitions + optional NVMe BDF/PCI IDs/class/Interrupt Line/BAR0 invariants when enabled)
- `crates/aero-machine/tests/machine_win7_storage_helper.rs` (helper preset wiring)
- `crates/aero-machine/tests/machine_win7_storage.rs` (helper constructor: PCI BDF presence + multifunction ISA bridge)
- `crates/aero-machine/tests/machine_disk_overlays_snapshot.rs` (snapshot `DISKS` disk_id mapping guard test)
- `crates/aero-machine/tests/machine_storage_snapshot_roundtrip.rs` (snapshot/restore: controller state + backend reattach contract)

---

## Summary (normative)

### Controllers present (canonical Win7 boot/install topology)

| Purpose | Controller | Canonical PCI BDF | Attachment |
|---|---|---:|---|
| Primary HDD (installed OS disk) | AHCI (Intel ICH9) | `aero_devices::pci::profile::SATA_AHCI_ICH9.bdf` (`00:02.0`) | **SATA port 0** (one disk) |
| Install media (boot ISO) | IDE (Intel PIIX3) | `aero_devices::pci::profile::IDE_PIIX3.bdf` (`00:01.1`) | **Secondary channel, master drive** (ATAPI CD-ROM) |

### Optional controllers (policy)

| Controller | Canonical PCI BDF | Win7 default policy |
|---|---:|---|
| NVMe (QEMU NVMe) | `aero_devices::pci::profile::NVME_CONTROLLER.bdf` (`00:03.0`) | **Off by default** (Win7 lacks inbox NVMe support) |

Rationale: Windows 7 requires hotfixes and/or vendor drivers for NVMe (e.g. KB2990941). For
compatibility-first defaults, keep NVMe disabled unless explicitly opted into by a config/feature.

Implementation note (Rust): `aero_machine::Machine::new_with_win7_storage(...)` /
`aero_machine::MachineConfig::win7_storage(...)` / `aero_machine::MachineConfig::win7_storage_defaults(...)`
are convenience helpers that enable this controller
set at the canonical BDFs for integration tests and bring-up.

These helpers keep non-storage devices conservative by default (for example, E1000 is disabled
unless explicitly enabled by the caller) to reduce drift and keep Win7 install/boot behavior
deterministic.

Lower-level platform-only tests may use `aero_pc_platform::PcPlatform::new_with_win7_storage(...)`
instead.

`aero_pc_platform::PcPlatform::new_with_windows7_storage_topology(...)` additionally attaches an
AHCI HDD (port 0) and an IDE/ATAPI CD-ROM (secondary master) so tests can validate real I/O.

`aero_machine::MachineConfig::win7_storage(...)` / `aero_machine::MachineConfig::win7_storage_defaults(...)`
(or `aero_machine::Machine::new_with_win7_storage(...)`)
enables the same canonical controller set in the full-system `Machine` integration layer.

---

## Boot flows (normative)

### BIOS boot drive numbering (DL) (normative)

Aero’s legacy BIOS follows PC-compatible drive numbering conventions when transferring control to a
boot sector / El Torito boot image:

| Medium | BIOS drive number | Notes |
|---|---:|---|
| First fixed disk (HDD0) | `DL=0x80` | Boot from the OS disk after install. |
| First ATAPI CD-ROM (CDROM0) | `DL=0xE0` | Boot from install/recovery ISO via El Torito. |

In the canonical machine topology, both an AHCI disk (HDD0) and an IDE/ATAPI CD-ROM (CD0) may be
attached simultaneously.

For BIOS INT 13h, sector units differ by drive number:

- HDD (`DL=0x80..=0xDF`): 512-byte sectors.
- CD-ROM (`DL=0xE0..=0xEF`): 2048-byte logical blocks via INT 13h Extensions (EDD).

Note: In the canonical machine topology, BIOS INT 13h can service **both** HDD0 (`DL=0x80`) and the
install-media CD-ROM (`DL=0xE0`) when both backends are provided (primary disk + install ISO). HDD
drive presence is derived from the BIOS Data Area fixed-disk count (`0x40:0x75`); when booting from
CD, the integration layer ensures this count still advertises HDD0 so guests can access it during
setup/boot.

### 1) Windows 7 install / recovery flow

1. Select **CD0** as the BIOS boot drive (**`DL=0xE0`**) *before* reset. In `aero_machine`, this is
    `Machine::set_boot_drive(0xE0)` followed by `Machine::reset()`. BIOS transfers control to the CD
    boot image with that CD drive number in `DL`.
    - Optional convenience: hosts may instead enable the firmware “CD-first when present” policy so
      firmware attempts a CD boot when install media is attached and otherwise falls back to the
      configured HDD boot drive (useful for “boot ISO once, then boot HDD after eject”):
      - Rust: `machine.set_cd_boot_drive(0xE0); machine.set_boot_from_cd_if_present(true);` (keep
        `boot_drive=0x80` as the fallback), then `machine.reset()`.
      - Config: `firmware::bios::BiosConfig::cd_boot_drive = 0xE0` +
        `firmware::bios::BiosConfig::boot_from_cd_if_present = true`.
      - Reporting: with this policy enabled, the configured boot drive/device remains the HDD
        fallback (e.g. `boot_drive=0x80` / `BootDevice::Hdd`) even when the current boot actually
        came from CD. Use `Machine::active_boot_device()` (or `Bios::booted_from_cdrom()`) to query
        what firmware actually booted from.
      - Rust convenience: `Machine::configure_win7_install_boot(iso)` (or
        `Machine::new_with_win7_install(...)`) enables this policy, attaches the ISO to the canonical
        ATAPI slot, and resets.
2. The boot ISO must be attached and presented as an **ATAPI CD-ROM** on **PIIX3 IDE secondary
   master** (`disk_id=1`) before BIOS POST/boot:
   - Rust: `Machine::attach_install_media_iso_bytes(...)` (in-memory ISO) or
     `Machine::attach_install_media_iso(...)` (file/stream-backed ISO).
   - Browser/wasm: `machine.attach_install_media_iso_bytes(...)` (copies bytes into WASM memory; OK
     for small ISOs) or `await machine.attach_install_media_iso_opfs(path)` (preferred for large
     ISOs; OPFS-backed, worker-only).
3. BIOS performs an **El Torito no-emulation** boot from that CD drive (see
   [`docs/09b-eltorito-cd-boot.md`](./09b-eltorito-cd-boot.md) for the detailed El Torito + INT 13h
   expectations).
4. If the CD is absent or unbootable (no ISO, empty tray, invalid boot catalog, etc.), the boot
   attempt will fail; the host should fall back by selecting HDD0 as the boot drive (**`DL=0x80`**)
   and resetting (i.e. configure `firmware::bios::BiosConfig::boot_drive = 0x80`, set
   `aero_machine::MachineConfig::boot_drive = 0x80` at construction time, or in `aero_machine` call
   `Machine::set_boot_drive(0x80)` and `Machine::reset()`).
   - Note: Aero’s BIOS boot selection is still primarily driven by an explicit `boot_drive` (`DL`),
     but it also supports an optional “CD-first when present” policy flag for host convenience.
5. Windows Setup enumerates the **AHCI disk** on **ICH9 AHCI port 0** and installs Windows onto it.

Implementation note (Rust): in `aero_machine`, the initial BIOS boot drive can be set up-front via
`MachineConfig::boot_drive` (for example `MachineConfig::win7_install_defaults(...)` sets
`boot_drive=0xE0` to boot directly from CD0, while the default is `boot_drive=0x80` for HDD boot).
You can also switch at runtime via `Machine::set_boot_drive(0xE0|0x80)` and then call
`Machine::reset()` to re-run BIOS POST with the new `DL` value.

Browser/wasm note: `crates/aero-wasm` exports `Machine` to JS and also exposes
`machine.set_boot_drive(0xE0|0x80)` alongside `machine.reset()`. The same rule applies: set the boot
drive number and then reset to re-run BIOS POST with the new `DL` value.

When available in the active WASM build, the JS-facing `Machine` wrapper also provides
introspection helpers for debugging/automation:

- `machine.boot_drive()` – configured BIOS boot drive (`DL`) used for firmware POST/boot.
- `machine.boot_from_cd_if_present()` – whether the firmware "CD-first when present" policy is enabled.
- `machine.cd_boot_drive()` – CD-ROM boot drive number used when the CD-first policy is enabled.
- `machine.active_boot_device()` – what firmware actually booted from in the current boot session (CD vs HDD).

In the browser runtime, the canonical `vmRuntime="machine"` mode runs the `Machine` inside a worker,
so hosts typically do not have direct access to a `machine` handle on the main thread. For stable
automation-friendly introspection, the runtime also exposes:

- `window.aero.debug.getMachineCpuActiveBootDevice()` – active boot source (CD vs HDD), or `null` if unknown.
- `window.aero.debug.getMachineCpuBootConfig()` – `{ bootDrive, cdBootDrive, bootFromCdIfPresent }`, or `null` if unknown.

### 2) Normal boot (after installation)

1. Select HDD0 as the BIOS boot drive (**`DL=0x80`**) before reset. BIOS enters the boot sector with
   **`DL=0x80`**.
2. The IDE CD-ROM may remain attached (useful for tooling/driver ISOs), but is not required.

---

## Media attachment mapping (normative)

### AHCI HDD mapping

- The **primary VM disk image** (the installed OS disk) is attached to:
  - **Controller:** ICH9 AHCI (`SATA_AHCI_ICH9`)
  - **Port:** `0`
  - **Snapshot `disk_id`:** `0`

Notes:
- The AHCI device model can support multiple ports, but the **canonical Win7 topology** attaches
  exactly one disk at **port 0** (and typically instantiates the controller with a single
  implemented port) for determinism.
- Do not “move” the OS disk to another port without updating this document and any frontend/storage
  assumptions (e.g. “disk0 == AHCI port0”).

### IDE / ATAPI CD-ROM mapping

- The **Windows install ISO** (or other optical media) is attached to:
  - **Controller:** PIIX3 IDE (`IDE_PIIX3`)
  - **Channel:** secondary
  - **Drive:** master
  - **Snapshot `disk_id`:** `1`

This matches the explicit attachment API in the IDE model:
`aero_devices_storage::pci_ide::IdeController::attach_secondary_master_atapi(...)`.

Note: `IDE_PIIX3` is PCI function `00:01.1`. For OS enumeration to reliably discover it, the
platform should also expose the PIIX3 ISA bridge function (`aero_devices::pci::profile::ISA_PIIX3`
at `00:01.0`) with the multi-function bit set in its header type (see
`docs/pci-device-compatibility.md`).

---

## Snapshot DISKS mapping (DiskOverlayRefs) (normative)

Snapshots store disk/ISO *references* separately from device/controller state in the `DISKS`
section as `aero_snapshot::DiskOverlayRefs` entries (see [`docs/16-snapshots.md`](./16-snapshots.md)).
Each entry is a `DiskOverlayRef { disk_id, base_image, overlay_image }` keyed by a stable `disk_id`
(`u32`) that identifies a **logical attachment point** in the machine topology.

Note: `base_image` / `overlay_image` are opaque host identifiers and may be empty strings to
represent "not configured".

Browser/runtime convention (web machine runtime mode, `vmRuntime=machine` / `?vm=machine`):
`base_image` / `overlay_image` are interpreted as **OPFS-relative paths** (e.g.
`aero/disks/win7.img`). Paths are relative to the OPFS root (no leading `/`) and must not contain
`..` segments.

When restoring a snapshot, storage controller device snapshots intentionally restore only
guest-visible controller state and **drop any attached host backends** (disk files, ISO handles,
etc.). The host/runtime must:

1. Read the snapshot `DISKS` section.
2. Re-open the referenced base/overlay images on the host.
3. Re-attach those reopened backends to the canonical attachment points below **before** resuming
   guest execution.

### Canonical `disk_id` values for Win7

| `disk_id` | Attachment point | Purpose |
|---:|---|---|
| `0` | ICH9 AHCI, **SATA port 0** (`00:02.0`) | Primary HDD (installed OS disk) |
| `1` | PIIX3 IDE, **secondary channel master ATAPI** (`00:01.1`) | Install media ISO (CD-ROM) |
| `2` | PIIX3 IDE, **primary channel master ATA** (`00:01.1`) | Optional IDE primary master ATA disk |

This mapping is implemented as stable constants in the canonical machine integration layer:
`aero_machine::Machine::DISK_ID_*`.

These `disk_id` values are part of the Win7 platform ABI: changing them breaks deterministic
snapshot restore unless all producers/consumers are updated in lockstep.

Guard test: `crates/aero-machine/tests/machine_disk_overlays_snapshot.rs`.

---

## Legacy IDE compatibility expectations (normative)

Even though the IDE controller is a PCI function (`00:01.1`), the canonical topology exposes it in
**legacy compatibility mode** so classic BIOS/boot-loader software and Windows 7’s IDE/ATAPI stack
can talk to it via the fixed ISA-compatible I/O port map.

### Legacy port map (compat mode)

| Channel | Command block | Control block (alt status/devctl) | IRQ |
|---------|---------------|------------------------------------|-----|
| Primary | `0x1F0..=0x1F7` | `0x3F6..=0x3F7` | IRQ14 |
| Secondary | `0x170..=0x177` | `0x376..=0x377` | IRQ15 |

### PCI BAR expectations (PIIX3 IDE)

| BAR | Meaning | Expected base |
|-----|---------|---------------|
| BAR0 | Primary command block | `0x1F0` |
| BAR1 | Primary control block base (alt status/devctl at `+2`) | `0x3F4` |
| BAR2 | Secondary command block | `0x170` |
| BAR3 | Secondary control block base (alt status/devctl at `+2`) | `0x374` |
| BAR4 | Bus Master IDE (BMIDE), 16 bytes | default `0xC000` (relocatable) |

Note: `BAR1`/`BAR3` use the PCI IDE convention where the **alt status/devctl port** is at
`base + 2` (hence `0x3F4 + 2 = 0x3F6`, `0x374 + 2 = 0x376`).

### AHCI MMIO (ABAR) expectation

The ICH9 AHCI controller exposes the ABAR register block via **BAR5** (MMIO), size
`aero_devices::pci::profile::AHCI_ABAR_SIZE`.

The PCI config space register offset for the ABAR itself is
`aero_devices::pci::profile::AHCI_ABAR_CFG_OFFSET`.

---

## Interrupt routing expectations (normative)

### PCI INTx routing (AHCI / NVMe / IDE PCI config)

Aero’s canonical PCI INTx routing uses:

- `aero_devices::pci::irq_router::PciIntxRouterConfig::default()`
  - `PIRQ[A-D] -> GSI[10,11,12,13]`
- Root bus swizzle:
  - `PIRQ = (INTx + device_number) mod 4`

Since the PCI INTx storage controllers use `INTA#`, their routed GSIs are determined purely by the
PCI **device number**:

| Device | BDF | INTx pin | PIRQ | GSI |
|---|---:|---:|---:|---:|
| PIIX3 IDE | `00:01.1` | INTA | B | **11** |
| ICH9 AHCI | `00:02.0` | INTA | C | **12** |
| NVMe (optional) | `00:03.0` | INTA | D | **13** |

This table is validated by `crates/devices/tests/win7_storage_topology.rs`.

### Legacy IDE channel interrupts (ATA/ATAPI)

Even though PIIX3 IDE is a PCI function (`00:01.1`), the **data-plane** interrupts for legacy
IDE/ATAPI are the traditional ISA IRQs:

- **Primary channel:** ISA IRQ **14**
- **Secondary channel:** ISA IRQ **15**

In Aero’s platform interrupt controller (`aero_platform::interrupts::PlatformInterrupts`), ISA IRQs
map to GSIs of the same number by default (except IRQ0 → GSI2), so these correspond to:

- primary IDE: **GSI 14**
- secondary IDE: **GSI 15**

Note: `IDE_PIIX3.build_config_space()` still exposes a PCI `Interrupt Pin` (`INTA#`) and an
`Interrupt Line` consistent with `PciIntxRouterConfig::default()` (currently **11**), but the
canonical IDE/ATAPI completion interrupts that software relies on are IRQ14/IRQ15.
