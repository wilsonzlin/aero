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

---

## Summary (normative)

### Controllers present by default (Win7)

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

Implementation note (Rust): `aero_pc_platform::PcPlatform::new_with_win7_storage(...)` is a
convenience constructor that enables this controller set at the canonical BDFs for integration
tests and bring-up.

---

## Boot flows (normative)

### 1) Windows 7 install / recovery flow

1. BIOS boot order is configured to boot from **CD-ROM (El Torito)** first.
2. The boot ISO is presented as an **ATAPI CD-ROM** on **PIIX3 IDE secondary master**.
3. Windows Setup enumerates the **AHCI disk** on **ICH9 AHCI port 0** and installs Windows onto it.

### 2) Normal boot (after installation)

1. BIOS boot order boots from the **AHCI HDD** (ICH9 AHCI port 0).
2. The IDE CD-ROM may remain attached (useful for tooling/driver ISOs), but is not required.

---

## Media attachment mapping (normative)

### AHCI HDD mapping

- The **primary VM disk image** (the installed OS disk) is attached to:
  - **Controller:** ICH9 AHCI (`SATA_AHCI_ICH9`)
  - **Port:** `0`

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

This matches the explicit attachment API in the IDE model:
`aero_devices_storage::pci_ide::IdeController::attach_secondary_master_atapi(...)`.

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
