# Workstream H: Integration & Boot

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

This workstream owns **system integration**: BIOS, ACPI tables, device model wiring, PCI bus, interrupt controllers, timers, and the overall boot sequence.

This is the **coordination hub**. You wire together the work from all other workstreams and make the system boot.

## Current status (implementation reality)

### Implemented (already in-tree)

- **Legacy BIOS (HLE) with INT dispatch via ROM stubs + `HLT` hypercall**:
  `crates/firmware/src/bios/mod.rs` (see module-level docs) + ROM generation in
  `crates/firmware/src/bios/rom.rs`.
- **POST + boot handoff** (IVT/BDA/EBDA init, A20 enable, then either MBR `0000:7C00` or El Torito
  no-emulation image depending on `BiosConfig::boot_drive`):
  `crates/firmware/src/bios/post.rs`, `crates/firmware/src/bios/ivt.rs`.
- **E820 map with PCI holes + >4 GiB high-memory remap**:
  `crates/firmware/src/bios/interrupts.rs::build_e820_map` and PC constants in
  `crates/aero-pc-constants/src/lib.rs`.
- **ACPI table generation + publication during POST**:
  `crates/aero-acpi/` (tables + AML DSDT) and BIOS integration in
  `crates/firmware/src/bios/acpi.rs`.
- **PCI core + ECAM mapping** (config ports + 256 MiB ECAM window at `0xB000_0000`):
  `crates/devices/src/pci/*` (core types), platform wiring in
  `crates/aero-pc-platform/src/lib.rs` and `crates/aero-machine/src/lib.rs::map_pc_platform_mmio_regions`.
- **PC interrupt/timer models wired in canonical platforms**:
  PIC (8259A), LAPIC + I/O APIC, PIT (8254), RTC/CMOS, HPET, ACPI PM/Sci, IMCR
  (see `crates/devices/src/*`, `crates/aero-pc-platform/src/lib.rs`,
  `crates/aero-machine/src/lib.rs`).
- **PCI MSI/MSI-X message delivery (for devices that opt in)**:
  `aero_platform::interrupts::msi` + `PlatformInterrupts::trigger_msi`; used today by
  (non-exhaustive):
  - AHCI (MSI) and NVMe (MSI + single-vector MSI-X) in both `aero-machine` and `aero-pc-platform`
    (see `crates/aero-machine/src/lib.rs::{process_ahci,process_nvme}` and
    `crates/aero-pc-platform/src/lib.rs::{process_ahci,process_nvme}`), and
  - xHCI (MSI + single-vector MSI-X) in `aero-pc-platform` when enabled (see
    `crates/devices/src/usb/xhci.rs` and `crates/devices/tests/xhci_msi_integration.rs`), and
  - virtio-pci MSI-X delivery in canonical integrations (virtio-blk in both stacks;
    virtio-net/virtio-input in `aero-machine`) via real virtio interrupt sinks plus MSI-X
    enable/function-mask mirroring in `VirtioPciBar0Mmio` (see VTP-009).
- **Snapshots + restore plumbing**:
  format + tooling in `crates/aero-snapshot/`, IO device state in `crates/aero-io-snapshot/`,
  canonical machine integration/tests in `crates/aero-machine/tests/*`.

### Known major gaps / limitations (please don’t rediscover these)

- **SMP is still in bring-up (BSP-driven + partial AP execution; not a full SMP scheduler)**:
  `cpu_count` is **not** forced to 1: firmware publishes CPU topology via **ACPI MADT + SMBIOS**
  for `cpu_count >= 1`.
  - `aero_machine::Machine` includes basic SMP plumbing (per-vCPU LAPIC MMIO + INIT/SIPI bring-up +
    a bounded cooperative AP run loop inside `Machine::run_slice`; see
    `crates/aero-machine/tests/ap_tsc_sipi_sync.rs`, `lapic_mmio_per_vcpu.rs`,
    `ioapic_routes_to_apic1.rs`, `smp_lapic_timer_wakes_ap.rs`, and `smp_timer_irq_routed_to_ap.rs`.
  - `aero_machine::pc::PcMachine` / `aero_pc_platform::PcPlatform` remain **BSP-only execution**
    today; `cpu_count > 1` there is still primarily for firmware-table enumeration tests.
  Full SMP work remains substantial (stable multi-vCPU scheduling, AP↔BSP/AP IPI paths, and
  determinism/snapshot/time integration).
  - **Workaround (for real guest boots today):** keep `cpu_count = 1` and use snapshots for fast
    boot/dev workflows (see [`docs/16-snapshots.md`](../docs/16-snapshots.md)).
  - **Progress tracker / plan:** [`docs/21-smp.md`](../docs/21-smp.md)
  See [`docs/09-bios-firmware.md#smp-boot-bsp--aps`](../docs/09-bios-firmware.md#smp-boot-bsp--aps).
- **Virtio legacy INTx delivery is polling-based (MSI-X is wired end-to-end)**:
  - Transport MSI-X support (table/PBA + vector programming): `crates/aero-virtio/src/pci.rs`.
  - MSI-X enable/function-mask bits are mirrored into the virtio transport (`sync_virtio_msix_from_platform`,
    `VirtioPciBar0Mmio::{sync_pci_config,sync_pci_command}`); MSI delivery uses
    `VirtioPlatformInterruptSink` / `VirtioMsixInterruptSink`.
  - Coverage: `crates/aero-machine/tests/virtio_blk_msix.rs`,
    `crates/aero-machine/tests/virtio_input_msix.rs`,
    `crates/aero-machine/tests/machine_snapshot_preserves_msix_enable.rs`,
    `crates/aero-pc-platform/tests/pc_platform_virtio_blk_msix.rs`,
    `crates/aero-pc-platform/tests/pc_platform_virtio_blk_msix_snapshot.rs`.
- **NVMe MSI/MSI-X is implemented (but Win7 support is opt-in/experimental)**:
  `aero-devices-nvme` exposes MSI + MSI-X capabilities (currently single-vector MSI-X) and delivers
  message-signaled interrupts when enabled (see `crates/aero-devices-nvme/README.md`,
  `crates/aero-devices-nvme/tests/interrupts.rs`, `crates/aero-machine/tests/nvme_msix.rs`, and
  `pc_platform_nvme` tests).
  Note: Windows 7 has no in-box NVMe driver.
- **MSI/MSI-X delivery is LAPIC-based; in legacy PIC mode MSI vectors are not surfaced to the CPU**:
  `PlatformInterrupts::trigger_msi` decodes the MSI address/data and injects a fixed interrupt into
  the selected LAPIC(s) (destination ID `0xFF` broadcasts to all LAPICs; see
  `crates/platform/src/interrupts/msi.rs`,
  `crates/platform/src/interrupts/router.rs::{inject_fixed_for_apic,inject_fixed_broadcast}`, and
  `crates/platform/tests/smp_msi_routing.rs`, and
  `crates/devices/tests/msix_cpu_core_integration.rs`).
  Note: MSI injection intentionally bypasses `PlatformInterruptMode`, but while the platform is in
  **Legacy PIC mode** the vCPU interrupt polling path (`InterruptController::get_pending` /
  `PlatformInterrupts::{get_pending_for_cpu,get_pending_for_apic}`) consults the 8259 PIC instead of
  LAPIC IRR state, so MSI-delivered vectors will not be observed until the guest switches to APIC
  mode. Also ensure the guest leaves the LAPIC software-enabled (SVR[8]=1).
---

## Key Crates & Directories

The **canonical** integration stack is now fully in-tree: BIOS/ACPI are implemented in Rust, and the
machine wiring lives in `aero-machine` (see [ADR 0014](../docs/adr/0014-canonical-machine-stack.md)).
Older docs may still read like “bring your own BIOS binary” or like the machine layer is still TBD
— that is no longer accurate.

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-machine/` | **Canonical machine integration layer** (ADR 0014): CPU + memory + devices + firmware; dispatches BIOS interrupt “hypercalls” |
| `crates/firmware/` | **Canonical legacy BIOS HLE** (`firmware::bios`): ROM stub generation + POST + INT services; publishes ACPI + SMBIOS |
| `crates/aero-acpi/` | ACPI table generator (used by `firmware::bios`) |
| `crates/devices/` | Device models (PCI, PIT/RTC/HPET, PIC port wrappers, AHCI/IDE, etc.) |
| `crates/platform/` / `crates/aero-pc-platform/` | Platform buses + PC/Q35-ish wiring helpers used by `aero-machine` |
| `crates/aero-interrupts/` | Interrupt controller models (PIC/APIC/I/O APIC) used by the platform layer |
| `crates/aero-smp/` | Deterministic SMP model (per-vCPU state + LAPIC/IPI delivery + scheduler + snapshot integration) used by the legacy `crates/emulator/` SMP path (not yet wired into the canonical `aero-machine` execution loop). |
| `crates/aero-timers/` / `crates/aero-time/` | Legacy timer stack (currently not used by the canonical `aero-machine` / `aero-pc-platform` wiring; prefer `crates/devices/*` + `crates/platform/src/interrupts/*`) |
| `crates/aero-snapshot/` | VM snapshot/restore format + helpers |
| `crates/emulator/` | Legacy/native emulator runtime + compat stack (not canonical VM wiring). Some legacy integration surfaces (e.g. sandbox AeroGPU PCI wrapper + executor wiring) still live here. See [`docs/21-emulator-crate-migration.md`](../docs/21-emulator-crate-migration.md). |
| `crates/aero-boot-tests/` | QEMU-based reference boot-test harness (registers workspace-root `tests/` boot suites as `[[test]]` targets; see [`docs/TESTING.md`](../docs/TESTING.md)) |
| `assets/bios.bin` | **Generated fixture**: a 64 KiB ROM image built from `firmware::bios::build_bios_rom()` (not the runtime BIOS source). Regenerate with `cargo xtask bios-rom` (or `cargo xtask fixtures`). |
| `tests/fixtures/boot/` | Deterministic tiny boot fixtures generated by `cargo xtask fixtures` (CI enforces determinism via `cargo xtask fixtures --check`) |
| `tests/boot/basic_boot.rs`, `tests/boot_sector.rs`, `tests/freedos_boot.rs`, `tests/windows7_boot.rs` | QEMU-based reference boot tests (registered under `crates/aero-boot-tests`) |
| `scripts/prepare-freedos.sh` | Downloads + patches FreeDOS image into `test-images/` (required for `freedos_boot`) |
| `scripts/validate-acpi.sh` | Validates the checked-in AML tables under `crates/firmware/acpi/` using ACPICA `iasl` |

---

## Essential Documentation

**Must read:**

- [`docs/09-bios-firmware.md`](../docs/09-bios-firmware.md) — BIOS and ACPI
- [`docs/09b-eltorito-cd-boot.md`](../docs/09b-eltorito-cd-boot.md) — El Torito CD boot + INT 13h extensions (Win7 install media)
- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md) — System architecture
- [`docs/16-snapshots.md`](../docs/16-snapshots.md) — Snapshot format
- [`docs/05-storage-topology-win7.md`](../docs/05-storage-topology-win7.md) — Canonical Windows 7 storage topology (stable PCI BDFs + media attachment mapping + IRQ routing)
- [`docs/TESTING.md`](../docs/TESTING.md) — How to run CI-equivalent tests locally (includes QEMU boot tests + fixtures)

**Reference:**

- [`docs/12-testing-strategy.md`](../docs/12-testing-strategy.md) — Integration testing
- [`docs/14-project-milestones.md`](../docs/14-project-milestones.md) — Boot milestones
- [`docs/16-debugging-and-introspection.md`](../docs/16-debugging-and-introspection.md) — Debug surfaces

---

## Tasks (status-aware)

Most `BI-*` / `AC-*` / `DM-*` items from the original project plan are now **implemented and covered
by tests** in the canonical stack. The tables below reflect current reality and point at the
relevant crates/tests.

Status legend:

- **Implemented**: in-tree + covered by unit/integration tests.
- **Partial**: implemented, but with a known limitation called out in the Notes/Pointers.
- **Open**: not implemented yet (or implemented in one integration layer but not the other).

### BIOS Tasks

> Note: The canonical Windows 7 storage + boot-media topology (including the El Torito install
> flow) is defined in [`docs/05-storage-topology-win7.md`](../docs/05-storage-topology-win7.md) and
> [`docs/09b-eltorito-cd-boot.md`](../docs/09b-eltorito-cd-boot.md). In the current BIOS, the boot
> path is **selected by the host** via `BiosConfig::boot_drive` (typically `0xE0` for install media,
> `0x80` for normal HDD boot).

| ID | Task | Status | Priority | Dependencies | Complexity | Pointers |
|----|------|--------|----------|--------------|------------|----------|
| BI-001 | POST sequence | Implemented | P0 | None | Medium | `crates/firmware/src/bios/post.rs` |
| BI-002 | Memory detection (E820) | Implemented | P0 | BI-001 | Medium | `crates/firmware/src/bios/interrupts.rs::build_e820_map` |
| BI-003 | Interrupt vector table setup | Implemented | P0 | BI-001 | Low | `crates/firmware/src/bios/ivt.rs` |
| BI-004 | BIOS data area setup | Implemented | P0 | BI-001 | Low | `crates/firmware/src/bios/ivt.rs::init_bda` |
| BI-005 | INT 10h (video) | Implemented | P0 | None | Medium | `crates/firmware/src/bios/int10.rs`, `int10_vbe.rs` |
| BI-006 | INT 13h (disk + EDD + CD-ROM + El Torito services) | Implemented | P0 | None | Medium | `crates/firmware/src/bios/interrupts.rs::handle_int13`, `crates/firmware/src/bios/eltorito.rs` (note: BIOS disk backend is currently read-only; write ops report write-protected) |
| BI-007 | INT 15h (system) | Implemented | P0 | None | Medium | `crates/firmware/src/bios/interrupts.rs::handle_int15` |
| BI-008 | INT 16h (keyboard) | Implemented | P0 | None | Low | `crates/firmware/src/bios/interrupts.rs::handle_int16` |
| BI-009 | Boot device selection (host-configured via `BiosConfig::boot_drive`) | Implemented | P0 | BI-006 | Low | `firmware::bios::BiosConfig::boot_drive`, `crates/firmware/src/bios/post.rs::boot` |
| BI-010 | Boot code loading (HDD MBR/VBR or El Torito no-emulation image) | Implemented | P0 | BI-009 | Low | `crates/firmware/src/bios/post.rs::boot_eltorito` |
| BI-011 | BIOS test suite | Implemented | P0 | BI-001..BI-010 | Medium | Run `cargo test -p firmware` (see tests in `crates/firmware/src/bios/*`) |

### ACPI Tasks

| ID | Task | Status | Priority | Dependencies | Complexity | Pointers |
|----|------|--------|----------|--------------|------------|----------|
| AC-001 | RSDP/RSDT/XSDT generation | Implemented | P0 | None | Medium | `crates/aero-acpi/src/tables.rs`, `crates/firmware/src/bios/acpi.rs` |
| AC-002 | FADT (Fixed ACPI Description Table) | Implemented | P0 | AC-001 | Medium | `crates/aero-acpi/src/tables.rs` |
| AC-003 | MADT (Multiple APIC Description Table) | Implemented | P0 | AC-001 | Medium | `crates/aero-acpi/src/tables.rs` |
| AC-004 | HPET table | Implemented | P0 | AC-001 | Low | `crates/aero-acpi/src/tables.rs` |
| AC-005 | DSDT (AML bytecode) | Implemented (minimal) | P1 | AC-001 | High | `crates/aero-acpi/src/tables.rs` (AML builder) |
| AC-006 | Power management stubs | Implemented (ACPI PM device) | P1 | AC-002 | Medium | `crates/devices/src/acpi_pm.rs` + FADT/DSDT in `aero-acpi` |
| AC-007 | ACPI test suite | Implemented | P0 | AC-001..AC-006 | Medium | Run `cargo test -p aero-acpi` (see `crates/aero-acpi/tests/*`) |

### Device Models Tasks

| ID | Task | Status | Priority | Dependencies | Complexity | Pointers |
|----|------|--------|----------|--------------|------------|----------|
| DM-001 | PIC (8259A) | Implemented | P0 | None | Medium | Core model: `crates/aero-interrupts/src/pic8259.rs`; port wrapper + platform registration: `crates/devices/src/pic8259.rs` |
| DM-002 | PIT (8254) | Implemented | P0 | None | Medium | `crates/devices/src/pit8254.rs` |
| DM-003 | CMOS/RTC | Implemented | P0 | None | Medium | `crates/devices/src/rtc_cmos.rs` |
| DM-004 | Local APIC | Implemented (multi-LAPIC topology; BSP-centric delivery) | P0 | None | High | Core model: `crates/aero-interrupts/src/apic/local_apic.rs`; routing glue in `crates/platform/src/interrupts/router.rs`; MMIO adapters in `crates/aero-machine/src/lib.rs` |
| DM-005 | I/O APIC | Implemented | P0 | DM-004 | High | Core model: `crates/aero-interrupts/src/apic/io_apic.rs`; routing glue in `crates/platform/src/interrupts/router.rs`; MMIO adapters in `crates/aero-machine/src/lib.rs` |
| DM-006 | HPET | Implemented | P0 | None | Medium | `crates/devices/src/hpet.rs` |
| DM-007 | PCI configuration space | Implemented | P0 | None | High | `crates/devices/src/pci/*` |
| DM-008 | PCI device enumeration | Implemented | P0 | DM-007 | Medium | `aero_devices::pci::bios_post` + `crates/aero-machine/tests/win7_storage_topology.rs` |
| DM-009 | DMA controller (8237) | Implemented (stub) | P1 | None | Medium | `crates/devices/src/dma.rs` |
| DM-010 | Serial port (16550) | Implemented | P2 | None | Medium | `crates/devices/src/serial.rs` |
| DM-011 | Device models test suite | Implemented | P0 | DM-001..DM-010 | Medium | `cargo test -p aero-devices`, `cargo test -p aero-pc-platform`, `cargo test -p aero-machine` |

### Virtio PCI Transport Tasks

| ID | Task | Status | Priority | Dependencies | Complexity | Pointers |
|----|------|--------|----------|--------------|------------|----------|
| VTP-001 | Virtio core (virtqueue, feature negotiation) | Implemented | P0 | DM-007 | High | `crates/aero-virtio/src/queue.rs`, `crates/aero-virtio/src/devices/*` |
| VTP-002 | Virtio PCI modern transport | Implemented | P0 | VTP-001, DM-007 | High | `crates/aero-virtio/src/pci.rs` |
| VTP-003 | Virtio PCI legacy transport | Implemented | P0 | VTP-001, DM-007 | High | `crates/aero-virtio/src/pci.rs` |
| VTP-004 | Virtio PCI transitional device | Implemented | P0 | VTP-002, VTP-003 | Medium | `VirtioPciDevice::new_transitional` |
| VTP-005 | Legacy INTx wiring | Implemented | P0 | VTP-003 | Medium | `VirtioPciDevice::irq_level()` + platform INTx routers |
| VTP-006 | MSI-X support | Implemented | P1 | VTP-002, DM-007 | High | Transport MSI-X logic: `crates/aero-virtio/src/pci.rs` (`MsixCapability` + `InterruptSink::signal_msix`). Platform wiring: `crates/aero-pc-platform/src/lib.rs::VirtioPciBar0Mmio::sync_pci_config` + `sync_virtio_msix_from_platform`, and `crates/aero-machine/src/lib.rs::VirtioPciBar0Mmio::sync_pci_command` + `sync_virtio_msix_from_platform` (see tests `crates/aero-pc-platform/tests/pc_platform_virtio_blk_msix*.rs` and `crates/aero-machine/tests/virtio_blk_msix.rs`). |
| VTP-007 | Unit tests | Implemented | P0 | VTP-003 | Medium | `cargo test -p aero-virtio` (see `crates/aero-virtio/tests/*`) |
| VTP-008 | Config option: disable modern | Implemented | P1 | VTP-004 | Low | `VirtioPciOptions::{modern_only,legacy_only,transitional}` |
| VTP-009 | Wire virtio MSI/MSI-X into canonical machine/platform | Implemented | P1 | VTP-006 | High | `aero_pc_platform`: `VirtioPlatformInterruptSink` delivers `MsiMessage` into `PlatformInterrupts::trigger_msi`; `VirtioPciBar0Mmio::sync_pci_config` mirrors PCI command + MSI-X enable/mask into the virtio transport. `aero_machine`: virtio-net/virtio-blk/virtio-input use `VirtioMsixInterruptSink` to deliver `MsiMessage`; `VirtioPciBar0Mmio::sync_pci_command` mirrors PCI command + MSI-X enable/mask (see `crates/aero-machine/tests/virtio_blk_msix.rs` and `crates/aero-machine/tests/virtio_input_msix.rs`). `NoopVirtioInterruptSink` is only used for configurations/devices without interrupt delivery (i.e. `Machine` has no `PlatformInterrupts`). |

### Canonical machine/platform gaps (actionable)

| ID | Task | Priority | Complexity | Notes / entry points |
|----|------|----------|------------|----------------------|
| MP-001 | SMP: run multiple vCPUs (make bring-up usable for real SMP guests) | P0 | Very High | `cpu_count > 1` is accepted and published via **ACPI MADT + SMBIOS**. `aero_machine::Machine` has basic SMP scaffolding (per-vCPU LAPIC MMIO, INIT/SIPI bring-up, and a cooperative AP run loop), but it is not yet a full SMP scheduler. Remaining work is robust multi-vCPU scheduling/execution (fairness/parallelism), AP↔AP/BSP IPI delivery from guest code, per-vCPU external interrupt injection, and snapshot/time determinism across multiple cores. |
| MP-002 | MSI/MSI-X: unify config-state mirroring in canonical PCI integrations | P1 | High | Message delivery exists (`PlatformInterrupts::trigger_msi`) and is used by AHCI (MSI), NVMe (MSI/MSI-X), xHCI (MSI), and virtio (MSI-X). The remaining integration pain is **keeping device-internal capability state coherent** with the canonical PCI config space (`PciConfigPorts`) (e.g. mirroring MSI/MSI-X enable/mask state into device models when the platform owns PCI config space). Snapshot regression: `crates/aero-machine/tests/machine_snapshot_preserves_msix_enable.rs`. |

If you are looking for impactful integration/boot work today, focus on:

- **SMP / multi-vCPU bring-up** (MP-001)
- **PCI routing hardening**: keep `aero_acpi` DSDT `_PRT`, PCI “Interrupt Line” programming, and the
  runtime INTx/MSI/MSI-X delivery model coherent and snapshot-safe.
- **Snapshot determinism & stability**: ensure device ordering, guest time, and interrupt state are
  reproducible across save/restore (see `docs/16-snapshots.md` and `crates/aero-machine/tests/`).

---

## Boot Sequence

### Phase 1: BIOS

```
Power On
    │
    ▼
POST (Power On Self Test)
    │
    ▼
Memory Detection (E820)
    │
    ▼
Interrupt Vector Table Setup
    │
    ▼
BIOS Data Area Setup
    │
    ▼
Boot Device Selection
    │
    ▼
Load boot code:
  - If `boot_drive` is `0xE0..=0xEF`: El Torito CD-ROM boot image (no-emulation) (Win7 install media; see `docs/09b-eltorito-cd-boot.md`)
  - Otherwise: MBR / boot sector (HDD/floppy; 512-byte sector)
    │
    ▼
Jump to loaded boot code
```

### Phase 2: Boot Loader (Windows)

```
Boot Sector (bootmgr)
    │
    ▼
Switch to Protected Mode
    │
    ▼
Load winload.exe
    │
    ▼
Switch to Long Mode (64-bit)
    │
    ▼
Load ntoskrnl.exe + HAL
    │
    ▼
Kernel Initialization
    │
    ▼
Desktop (explorer.exe)
```

---

## Memory Map

PC/Q35 memory map that BIOS must report via E820 (source of truth:
`crates/firmware/src/bios/interrupts.rs::build_e820_map`, constants in
`crates/aero-pc-constants/src/lib.rs`):

```
0x0000_0000 - 0x0009_EFFF   636 KiB   Conventional memory (usable)
0x0009_F000 - 0x0009_FFFF     4 KiB   EBDA (reserved)
0x000A_0000 - 0x000F_FFFF   384 KiB   VGA/BIOS/option ROM window (reserved)

0x0010_0000 - 0xB000_0000   ...       Low RAM (usable; clamped to ECAM base)

0xB000_0000 - 0xC000_0000   256 MiB   PCIe ECAM / MMCONFIG (reserved)
                              - `aero_pc_constants::PCIE_ECAM_BASE = 0xB000_0000`
                              - `PCIE_ECAM_SIZE = 0x1000_0000`

0xC000_0000 - 0x1_0000_0000  1 GiB    PCI/MMIO hole (reserved; PCI BARs, APIC/HPET, etc.)

0x1_0000_0000 - ...          ...      High RAM remap (usable, only when RAM > 0xB000_0000)
                                    High RAM length = `total_ram - 0xB000_0000`
```

When the configured guest RAM size exceeds `PCIE_ECAM_BASE` (`0xB000_0000`), the BIOS reserves the
ECAM window (`0xB000_0000..0xC000_0000`) and the PCI/MMIO hole (`0xC000_0000..0x1_0000_0000`) in
the E820 map. To preserve the configured RAM size, the remainder is remapped above 4 GiB starting
at `0x1_0000_0000`.

This implies the emulator’s RAM backend must be **hole-aware**: guest RAM is not a single
contiguous `[0, total_ram)` region once PCI holes are modeled. Physical addresses in the reserved
holes must not hit RAM (and if not claimed by an MMIO device, should behave like open bus reads:
`0xFF` bytes / all-ones).

Implementation note: in the Rust VM core, this is modeled via `memory::MappedGuestMemory`
(`crates/memory/src/mapped.rs`) and is already applied by the canonical PC memory buses when
`ram_size > PCIE_ECAM_BASE` (see `crates/platform/src/memory.rs::MemoryBus::wrap_pc_high_memory`
and `crates/aero-machine/src/lib.rs::SystemMemory::new`).

---

## Interrupt Routing

### Legacy (PIC)

```
IRQ 0  - PIT Timer
IRQ 1  - Keyboard
IRQ 2  - Cascade (PIC2)
IRQ 3  - Serial COM2
IRQ 4  - Serial COM1
IRQ 5  - LPT2 / Sound
IRQ 6  - Floppy
IRQ 7  - LPT1
IRQ 8  - RTC
IRQ 9  - Redirected IRQ2
IRQ 10 - Available
IRQ 11 - Available
IRQ 12 - PS/2 Mouse
IRQ 13 - FPU
IRQ 14 - Primary IDE
IRQ 15 - Secondary IDE
```

### APIC Mode

Windows 7 prefers APIC. The MADT tells the OS about APIC configuration:
- Local APIC ID for each CPU
- I/O APIC address and GSI base
- Interrupt source overrides (e.g., IRQ0 → GSI2)

---

## PCI Device Enumeration

The canonical PCI layout (BDFs, IDs/class codes, and INTx routing) is defined in:

- [`docs/pci-device-compatibility.md`](../docs/pci-device-compatibility.md)
- `aero_devices::pci::profile` (source of truth for the constants used by tests/platform code)

**Important:** The canonical Windows 7 storage topology is normative and requires **ICH9 AHCI at
`00:02.0`** (see [`docs/05-storage-topology-win7.md`](../docs/05-storage-topology-win7.md)). Do not
assign any other device to `00:02.0`.

### Canonical bus 0 device numbers (when enabled)

This is the canonical BDF reservation map from `aero_devices::pci::profile` (used by tests and the
Windows driver/device contract). **Not every entry is currently wired into `aero_machine::Machine`**
yet; for the authoritative “what can the canonical machine expose today”, see
`crates/aero-machine/src/lib.rs::MachineConfig` (feature flags like `enable_ahci`, `enable_nvme`,
`enable_virtio_*`, `enable_ehci`, etc).

```
00:00.0  - Host bridge (Q35)
00:1f.0  - ISA/LPC bridge (ICH9)

00:01.0  - PIIX3 ISA bridge (multi-function; enables 00:01.1/00:01.2 discovery)
00:01.1  - PIIX3 IDE (Win7 install ISO attachment; legacy compat mode)
00:01.2  - PIIX3 UHCI (USB 1.1)

00:02.0  - ICH9 AHCI (Win7 OS disk; canonical and normative)
00:03.0  - NVMe (optional; off by default for Win7)
00:04.0  - HD Audio (ICH6)
00:05.0  - E1000 NIC
00:06.0  - RTL8139 NIC (alternate)
00:07.0  - AeroGPU display controller (reserved canonical BDF; `PCI\VEN_A3A0&DEV_0001`, see `docs/abi/aerogpu-pci-identity.md`)
00:08.0  - virtio-net
00:09.0  - virtio-blk
00:0a.0  - virtio-input keyboard (multi-function)
00:0a.1  - virtio-input mouse
00:0b.0  - virtio-snd
00:0c.0  - VGA (stub) Bochs/QEMU “Standard VGA” (`1234:1111`; legacy VGA mode only)
00:0d.0  - xHCI (USB 3.x) controller (optional/experimental; Win7 has no in-box xHCI driver; see `docs/usb-xhci.md`)
00:12.0  - EHCI (USB 2.0) controller (optional; Win7 in-box `usbehci.sys`; see `docs/usb-ehci.md`)
```

### Note on VGA / display today

With `MachineConfig::enable_vga=true` (and `enable_aerogpu=false`), the canonical
`aero_machine::Machine` uses `aero_gpu_vga` (VGA + Bochs VBE_DISPI) for boot display:

* Legacy VGA ports: `0x3B0..0x3DF` (includes both mono and color decode ranges; common subset `0x3C0..0x3DF`)
* VBE ports: `0x01CE/0x01CF`
* Legacy VRAM window: `0xA0000..0xBFFFF`
* SVGA linear framebuffer (LFB): base is **configurable** (historically defaulting to `0xE000_0000`
  via `aero_gpu_vga::SVGA_LFB_BASE`).
  - When `MachineConfig::enable_pc_platform=true`, the canonical machine also exposes a
    Bochs/QEMU-compatible “Standard VGA” PCI stub at `00:0c.0` (`1234:1111`) and routes the LFB
    through BAR0 inside the ACPI-reported PCI MMIO window. The BAR base is assigned by BIOS POST /
    the PCI resource allocator unless pinned via `MachineConfig::{vga_lfb_base,vga_vram_bar_base}`.
  - When `MachineConfig::enable_pc_platform=false`, the machine maps the LFB MMIO aperture directly
    at the configured base.

This legacy VGA/VBE boot-display path intentionally does *not* occupy `00:07.0`: that BDF is
reserved for the long-term AeroGPU WDDM device identity (`PCI\VEN_A3A0&DEV_0001`; see
[`docs/abi/aerogpu-pci-identity.md`](../docs/abi/aerogpu-pci-identity.md) and
[`docs/16-aerogpu-vga-vesa-compat.md`](../docs/16-aerogpu-vga-vesa-compat.md)).

With `MachineConfig::enable_aerogpu=true` (and `enable_vga=false`), the canonical machine exposes
the AeroGPU PCI identity (`A3A0:0001`) at `00:07.0` with the canonical BAR layout (BAR0 regs + BAR1
VRAM aperture) for stable Windows driver binding and enumeration. In `aero_machine` today BAR1 is
backed by a dedicated VRAM buffer and implements permissive legacy VGA decode (VGA port I/O +
VRAM-backed `0xA0000..0xBFFFF` window; see `docs/16-aerogpu-vga-vesa-compat.md`).

BAR0 is implemented as a **minimal MMIO + ring/fence transport + submission decode/capture** surface.
In the default/no-backend mode, `aero_machine` completes fences without executing ACMD so the in-tree
Win7 KMD can initialize and make forward progress even when no executor is available.

Command execution can be supplied by host-side executors/backends:

- Browser runtime: out-of-process GPU worker execution via the WASM “submission bridge”
  (`Machine::aerogpu_drain_submissions` / `Machine::aerogpu_complete_fence`, exported from
  `crates/aero-wasm`).
- Native builds/tests: optional in-process backends (including a feature-gated headless wgpu backend
  via `Machine::aerogpu_set_backend_wgpu`).

`aero_machine` also implements scanout0 + vblank register storage (including vblank
counters/timestamps and IRQ semantics) and a host presentation path that can read/present the
guest-programmed scanout framebuffer via `Machine::display_present`.

Shared device-side building blocks (regs/ring/executor + reusable PCI wrapper) live in
`crates/aero-devices-gpu`, with a legacy/sandbox integration surface still in `crates/emulator`.

When AeroGPU owns the boot display path, firmware derives the VBE mode-info linear framebuffer base
from AeroGPU BAR1 (`PhysBasePtr = BAR1_BASE + 0x40000`, aka `VBE_LFB_OFFSET` in `aero_machine` /
`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES` in the protocol).

In this mode no transitional VGA PCI stub is installed.

The transitional VGA/VBE path is a boot-display stepping stone and does **not** implement the full
AeroGPU WDDM MMIO/ring protocol.

---

## Snapshot/Restore

Snapshots enable "instant boot":

1. Boot Windows 7 once (slow)
2. Save snapshot at desktop
3. Future sessions restore from snapshot (fast)

See [`docs/16-snapshots.md`](../docs/16-snapshots.md) for format.

---

## Debugging

### Serial Console

Enable serial output for debug messages:

```rust
// In BIOS or early boot code
fn debug_print(s: &str) {
    for b in s.bytes() {
        io_write(0x3F8, b as u64); // COM1
    }
}
```

### State Inspection

The emulator exposes CPU/device state for debugging. See [`docs/16-debugging-and-introspection.md`](../docs/16-debugging-and-introspection.md).

---

## Coordination Points

### What You Need From Other Workstreams

- **CPU (A)**: Working CPU modes, interrupt delivery
- **Graphics (B)**: VGA text mode for boot messages
- **Storage (D)**: AHCI/IDE for disk boot
- **Input (F)**: Keyboard for BIOS interaction
- **Audio (G)**: HD Audio for system sounds

### What Other Workstreams Need From You

- **All**: Working PCI bus, interrupt routing, timers
- **Drivers (C)**: Virtio PCI device models
- **Graphics (B)**: VGA BIOS INT 10h

---

## Testing

QEMU boot integration tests live under the workspace root `tests/` directory, but are registered
under the dedicated `aero-boot-tests` crate via `crates/aero-boot-tests/Cargo.toml` `[[test]]`
entries (e.g. `path = "../../tests/boot_sector.rs"`). Always run them via `-p aero-boot-tests`
(not `-p aero`).

```bash
# Regenerate/verify deterministic in-repo fixtures (BIOS ROM, ACPI DSDT, tiny boot images).
# CI runs `--check` and fails if any fixture is missing or out-of-date.
bash ./scripts/safe-run.sh cargo xtask fixtures
bash ./scripts/safe-run.sh cargo xtask fixtures --check

# Validate ACPI tables with ACPICA iasl (CI runs this in the `acpi-iasl` job).
# Requires `iasl` to be installed (ACPICA).
bash ./scripts/validate-acpi.sh

# Run BIOS tests
bash ./scripts/safe-run.sh cargo test -p firmware --locked

# Run ACPI table generator tests
bash ./scripts/safe-run.sh cargo test -p aero-acpi --locked

# Run device model tests
bash ./scripts/safe-run.sh cargo test -p aero-devices --locked
bash ./scripts/safe-run.sh cargo test -p aero-interrupts --locked
bash ./scripts/safe-run.sh cargo test -p aero-platform --locked

# Optional: legacy timer/time crates (not used by canonical machine/platform wiring today)
bash ./scripts/safe-run.sh cargo test -p aero-time --locked
bash ./scripts/safe-run.sh cargo test -p aero-timers --locked

# Run canonical integration tests (PCI wiring, snapshots, storage topologies, etc.)
bash ./scripts/safe-run.sh cargo test -p aero-pc-platform --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --locked
bash ./scripts/safe-run.sh cargo test -p aero-snapshot --locked
bash ./scripts/safe-run.sh cargo test -p aero-virtio --locked

# Boot tests (QEMU; requires qemu-system-i386 (or qemu-system-x86_64), mtools, unzip, curl)
# Note: the first `cargo test` in a clean/contended agent sandbox can take >10 minutes.
# If you hit safe-run timeouts during compilation, bump the timeout via AERO_TIMEOUT.
bash ./scripts/prepare-freedos.sh
# Ensure deterministic in-repo fixtures are present and up-to-date (boot sectors,
# BIOS ROM, ACPI DSDT, etc). CI enforces this via `cargo xtask fixtures --check`;
# no assembler toolchain required.
bash ./scripts/safe-run.sh cargo xtask fixtures --check
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-boot-tests --test boot_sector --locked
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-boot-tests --test freedos_boot --locked

# Full Windows 7 boot (local only; requires a user-supplied Windows 7 disk image)
bash ./scripts/prepare-windows7.sh
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-boot-tests --test windows7_boot --locked -- --ignored
```

---

## What’s missing to boot Windows 7? (checklist)

This checklist is intentionally **practical** and reflects what’s missing *in-repo* vs. what is
already implemented.

### A) Boot an existing Win7 installation (HDD image)

- [x] BIOS POST + INT services + ACPI/SMBIOS publication (`crates/firmware`, `crates/aero-acpi`)
- [x] Canonical PC platform wiring (interrupt controllers, timers, PCI, storage) (`crates/aero-machine`, `crates/aero-pc-platform`)
- [ ] **A Windows 7 disk image** (local-only; not in repo):
  - Put it at `test-images/local/windows7.img`, or set `AERO_WINDOWS7_IMAGE=/path/to/windows7.img`
  - Then run: `cargo test -p aero-boot-tests --test windows7_boot --locked -- --ignored` (see `scripts/prepare-windows7.sh`)
- [ ] Optional but recommended: provide a golden screenshot at `test-images/local/windows7_login.png` (or `AERO_WINDOWS7_GOLDEN=...`)

### B) Boot the Win7 installer from ISO (El Torito)

- [x] **BIOS El Torito no-emulation boot + INT 13h extensions** (what Win7 install ISOs use):
  see [`docs/09b-eltorito-cd-boot.md`](../docs/09b-eltorito-cd-boot.md) and `crates/firmware/src/bios/eltorito.rs`.
- [ ] **A Windows 7 install ISO** (local-only; not in repo):
  - In the agent sandbox this is usually available at `/state/win7.iso` (see `AGENTS.md`)
  - For slipstreaming drivers/certs/testsigning into install media, see `docs/16-windows7-install-media-prep.md`
- [ ] Host config: attach the ISO as the IDE/ATAPI CD-ROM and set BIOS `boot_drive` to `0xE0..=0xEF` (commonly `0xE0`).
  Normal HDD boot uses `0x80`.

### C) Important non-boot gaps

- [ ] SMP execution: SMP is still in bring-up. `aero_machine::Machine` has basic AP bring-up +
  cooperative AP execution, but it is not yet a full SMP scheduler. `aero_machine::pc::PcMachine` /
  `aero_pc_platform::PcPlatform` remain BSP-only execution today (MP-001).
- [ ] MSI/MSI-X hardening: message-signaled interrupts are wired for key devices (virtio, NVMe), but patterns/tests need to be generalized and made snapshot-safe as SMP lands (MP-002).

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `bash ./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/09-bios-firmware.md`](../docs/09-bios-firmware.md)
4. ☐ Read [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md)
5. ☐ Explore `crates/firmware/src/bios/`, `crates/aero-acpi/`, `crates/aero-machine/`, and `crates/aero-pc-platform/`
6. ☐ Regenerate/check deterministic in-repo fixtures: `bash ./scripts/safe-run.sh cargo xtask fixtures --check`
7. ☐ Run the in-process integration tests (`cargo test -p firmware`, `-p aero-machine`, `-p aero-pc-platform`, etc.)
8. ☐ Run QEMU reference boot tests (`boot_sector`, `freedos_boot`) after `bash ./scripts/prepare-freedos.sh`
9. ☐ Pick an item from the “Canonical machine/platform gaps” table (MP-001/MP-002) and begin

---

*Integration makes everything work together. You are the glue.*
