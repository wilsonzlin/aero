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

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/firmware/` | BIOS implementation |
| `crates/aero-acpi/` | ACPI table generation |
| `crates/platform/` | Platform wiring (PCI, interrupts, timers) |
| `crates/devices/` | Shared device model infrastructure |
| `crates/aero-interrupts/` | PIC, APIC, I/O APIC |
| `crates/aero-timers/` | PIT, HPET, RTC |
| `crates/aero-time/` | Time abstraction |
| `crates/aero-machine/` | Machine state, VM control |
| `crates/aero-snapshot/` | VM snapshot/restore |
| `crates/emulator/` | Main emulator wiring |
| `assets/bios.bin` | Compiled BIOS binary |

---

## Essential Documentation

**Must read:**

- [`docs/09-bios-firmware.md`](../docs/09-bios-firmware.md) — BIOS and ACPI
- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md) — System architecture
- [`docs/16-snapshots.md`](../docs/16-snapshots.md) — Snapshot format
- [`docs/05-storage-topology-win7.md`](../docs/05-storage-topology-win7.md) — Canonical Windows 7 storage topology (stable PCI BDFs + media attachment mapping + IRQ routing)

**Reference:**

- [`docs/12-testing-strategy.md`](../docs/12-testing-strategy.md) — Integration testing
- [`docs/14-project-milestones.md`](../docs/14-project-milestones.md) — Boot milestones
- [`docs/16-debugging-and-introspection.md`](../docs/16-debugging-and-introspection.md) — Debug surfaces

---

## Tasks

### BIOS Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| BI-001 | POST sequence | P0 | None | Medium |
| BI-002 | Memory detection (E820) | P0 | BI-001 | Medium |
| BI-003 | Interrupt vector table setup | P0 | BI-001 | Low |
| BI-004 | BIOS data area setup | P0 | BI-001 | Low |
| BI-005 | INT 10h (video) | P0 | None | Medium |
| BI-006 | INT 13h (disk) | P0 | None | Medium |
| BI-007 | INT 15h (system) | P0 | None | Medium |
| BI-008 | INT 16h (keyboard) | P0 | None | Low |
| BI-009 | Boot device selection | P0 | BI-006 | Low |
| BI-010 | MBR/boot sector loading | P0 | BI-009 | Low |
| BI-011 | BIOS test suite | P0 | BI-001..BI-010 | Medium |

### ACPI Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| AC-001 | RSDP/RSDT/XSDT generation | P0 | None | Medium |
| AC-002 | FADT (Fixed ACPI Description Table) | P0 | AC-001 | Medium |
| AC-003 | MADT (Multiple APIC Description Table) | P0 | AC-001 | Medium |
| AC-004 | HPET table | P0 | AC-001 | Low |
| AC-005 | DSDT (AML bytecode) | P1 | AC-001 | High |
| AC-006 | Power management stubs | P1 | AC-002 | Medium |
| AC-007 | ACPI test suite | P0 | AC-001..AC-004 | Medium |

### Device Models Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| DM-001 | PIC (8259A) | P0 | None | Medium |
| DM-002 | PIT (8254) | P0 | None | Medium |
| DM-003 | CMOS/RTC | P0 | None | Medium |
| DM-004 | Local APIC | P0 | None | High |
| DM-005 | I/O APIC | P0 | DM-004 | High |
| DM-006 | HPET | P0 | None | Medium |
| DM-007 | PCI configuration space | P0 | None | High |
| DM-008 | PCI device enumeration | P0 | DM-007 | Medium |
| DM-009 | DMA controller (8237) | P1 | None | Medium |
| DM-010 | Serial port (16550) | P2 | None | Medium |
| DM-011 | Device models test suite | P0 | DM-001..DM-006 | Medium |

### Virtio PCI Transport Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| VTP-001 | Virtio core (virtqueue, feature negotiation) | P0 | DM-007 | High |
| VTP-002 | Virtio PCI modern transport | P0 | VTP-001, DM-007 | High |
| VTP-003 | Virtio PCI legacy transport | P0 | VTP-001, DM-007 | High |
| VTP-004 | Virtio PCI transitional device | P0 | VTP-002, VTP-003 | Medium |
| VTP-005 | Legacy INTx wiring | P0 | VTP-003 | Medium |
| VTP-006 | MSI-X support | P1 | VTP-002, DM-007 | High |
| VTP-007 | Unit tests | P0 | VTP-003 | Medium |
| VTP-008 | Config option: disable modern | P1 | VTP-004 | Low |

---

## Boot Sequence

### Phase 1: BIOS

```
Power On
    │
    ▼
POST (Power On Self Test)           ← BI-001
    │
    ▼
Memory Detection (E820)             ← BI-002
    │
    ▼
Interrupt Vector Table Setup        ← BI-003
    │
    ▼
BIOS Data Area Setup                ← BI-004
    │
    ▼
Boot Device Selection               ← BI-009
    │
    ▼
Load MBR / Boot Sector              ← BI-010
    │
    ▼
Jump to Boot Sector (0x7C00)
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
00:0c.0  - Transitional VGA/VBE PCI stub (Bochs/QEMU "Standard VGA"-like identity; `aero_gpu_vga` LFB routing). Must not collide with AeroGPU at `00:07.0`.
```

### Note on VGA / display today

The canonical `aero_machine::Machine` currently uses `aero_gpu_vga` (VGA + Bochs VBE_DISPI) for
boot display:

* Legacy VGA ports: `0x3C0..0x3DF`
* VBE ports: `0x01CE/0x01CF`
* Legacy VRAM window: `0xA0000..0xBFFFF`
* Fixed SVGA linear framebuffer (LFB): `0xE000_0000` (within the reserved below-4 GiB PCI/MMIO hole)

To make the LFB reachable via the canonical PCI MMIO window (`0xE000_0000..`), the machine also
exposes a **minimal PCI VGA function** (Bochs/QEMU "Standard VGA"-like IDs) at `00:0c.0`.

This PCI stub is intentionally *not* at `00:07.0`: that BDF is reserved for the long-term AeroGPU
WDDM device identity (`PCI\VEN_A3A0&DEV_0001`; see
[`docs/abi/aerogpu-pci-identity.md`](../docs/abi/aerogpu-pci-identity.md) and
[`docs/16-aerogpu-vga-vesa-compat.md`](../docs/16-aerogpu-vga-vesa-compat.md)).

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
under the `emulator` crate via `crates/emulator/Cargo.toml` `[[test]]` entries (e.g.
`path = "../../tests/boot_sector.rs"`). Always run them via `-p emulator` (not `-p aero`).

```bash
# Run BIOS tests
bash ./scripts/safe-run.sh cargo test -p firmware --locked

# Run device model tests
bash ./scripts/safe-run.sh cargo test -p aero-devices --locked
bash ./scripts/safe-run.sh cargo test -p aero-interrupts --locked
bash ./scripts/safe-run.sh cargo test -p aero-timers --locked

# Boot tests
# Note: these integration tests live in the workspace root crate (`aero`) under `tests/`.
# Note: the first `cargo test` in a clean/contended agent sandbox can take >10 minutes.
# If you hit safe-run timeouts during compilation, bump the timeout via AERO_TIMEOUT.
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero --test boot_sector --locked
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero --test freedos_boot --locked

# Full Windows 7 boot (local only; requires a user-supplied Windows 7 disk image)
bash ./scripts/prepare-windows7.sh
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero --test windows7_boot --locked -- --ignored
```

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `bash ./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/09-bios-firmware.md`](../docs/09-bios-firmware.md)
4. ☐ Read [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md)
5. ☐ Explore `crates/firmware/src/` and `crates/platform/src/`
6. ☐ Run boot tests to see current state
7. ☐ Pick a task from the tables above and begin

---

*Integration makes everything work together. You are the glue.*
