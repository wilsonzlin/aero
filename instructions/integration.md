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

Standard PC memory map that BIOS must report via E820:

```
0x00000000 - 0x0009FFFF   640 KB    Conventional memory (usable)
0x000A0000 - 0x000BFFFF   128 KB    VGA memory (reserved)
0x000C0000 - 0x000FFFFF   256 KB    ROM area (reserved)
0x00100000 - 0xXXXXXXXX   ...       Extended memory (usable)
0xXXXXXXXX - 0xFEFFFFFF   ...       PCI MMIO (reserved)
0xFF000000 - 0xFFFFFFFF   16 MB     BIOS ROM (reserved)
```

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

Devices must be registered on the PCI bus:

```
Bus 0, Device 0  - Host Bridge
Bus 0, Device 1  - ISA Bridge (LPC)
Bus 0, Device 2  - AeroGPU (VGA)
Bus 0, Device 3  - AHCI Controller
Bus 0, Device 4  - E1000 NIC
Bus 0, Device 5  - HD Audio
Bus 0, Device 6  - Virtio-blk
Bus 0, Device 7  - Virtio-net
...
```

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

```bash
# Run BIOS tests
./scripts/safe-run.sh cargo test -p firmware --locked

# Run device model tests
./scripts/safe-run.sh cargo test -p devices --locked
./scripts/safe-run.sh cargo test -p aero-interrupts --locked
./scripts/safe-run.sh cargo test -p aero-timers --locked

# Boot tests
./scripts/safe-run.sh cargo test -p aero --test boot_sector --locked
./scripts/safe-run.sh cargo test -p aero --test freedos_boot --locked

# Full Windows 7 boot (requires ISO at /state/win7.iso)
./scripts/safe-run.sh cargo test -p aero --test windows7_boot --locked -- --ignored
```

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/09-bios-firmware.md`](../docs/09-bios-firmware.md)
4. ☐ Read [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md)
5. ☐ Explore `crates/firmware/src/` and `crates/platform/src/`
6. ☐ Run boot tests to see current state
7. ☐ Pick a task from the tables above and begin

---

*Integration makes everything work together. You are the glue.*
