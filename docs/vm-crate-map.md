# VM crate map (core wiring)

This repo historically accumulated multiple "VM" / "emulator" crates with overlapping goals. This document maps what exists today and (together with [ADR 0008](./adr/0008-canonical-vm-core.md)) establishes which crate is **canonical**.

## Canonical path (post-ADR-0008)

The canonical VM wiring crate is:

- `crates/aero-machine` (`aero-machine`) — `aero_machine::Machine`

Everything that wants to *run the Aero machine* (browser WASM exports, host integration tests, snapshot tooling) should build on that crate.

### High-level crate graph

```text
crates/aero-wasm      (wasm-bindgen JS API)
  ├── crates/aero-machine  (canonical machine wiring + stable API)
  │     ├── crates/aero-cpu-core  (Tier-0 interpreter + JIT ABI state)
  │     ├── crates/memory         (physical memory bus + guest memory backends)
  │     ├── crates/platform       (port I/O bus, chipset/reset wiring)
  │     ├── crates/devices        (core device models: serial/i8042/A20/reset)
  │     ├── crates/firmware       (BIOS HLE + ACPI/SMBIOS helpers)
  │     └── crates/aero-snapshot  (snapshot file format + save/restore machinery)
  └── (deprecated) `DemoVm` export is implemented as a thin wrapper around `aero-machine`
```

## Crate responsibilities (inventory)

### Canonical VM wiring

#### `crates/aero-machine` (`aero-machine`) — **canonical**

**What it does**
- Owns the *machine object* (`aero_machine::Machine`) and its stable public API:
  - machine config (`MachineConfig`)
  - run loop (`run_slice`, `RunExit`)
  - device attachment hooks (disk image, input injection, serial drain)
  - snapshot hooks (via `aero-snapshot`)

**Who should depend on it**
- `crates/aero-wasm` (browser/WASM exports)
- Host integration tests that need "a VM to run" (BIOS POST, boot sector smoke tests, snapshot determinism)

### Supporting building blocks

#### `crates/firmware` (`firmware`)

**What it does**
- Legacy BIOS implementation in Rust (POST + INT dispatch).
- Firmware table generation (ACPI, SMBIOS, E820).

**How it fits**
- Called by `aero-machine` during `Machine::reset()` (POST) and when the CPU triggers a BIOS interrupt hypercall.
-
**Note on the retired `crates/machine` harness**

Historically, the BIOS was typed on a separate `crates/machine` abstraction (`machine::CpuState`,
`machine::MemoryAccess`, etc). That harness has been retired; BIOS now runs directly on the
canonical CPU core state (`aero_cpu_core::state::CpuState`) and the canonical guest physical memory
bus (`memory::MemoryBus`), with a small set of firmware-local traits for ROM mapping, A20 control,
and block devices.

#### `crates/memory` (`memory`)

**What it does**
- Guest physical memory backends and the physical memory bus (`PhysicalMemoryBus`).

**How it fits**
- Used by `aero-machine` as the canonical physical address space implementation.

#### `crates/platform` (`aero-platform`) and `crates/devices` (`aero-devices`)

**What it does**
- Port I/O bus + chipset wiring (`aero-platform`) and reusable device models (`aero-devices`).

#### `crates/aero-pc-platform` (`aero-pc-platform`)

**What it does**
- Higher-level PC platform composition helper (PIC/PIT/RTC/APIC/HPET + PCI bus + BAR MMIO mapping).

**How it fits**
- This is a *platform builder* rather than the canonical VM object itself.
- It is used by targeted platform/unit tests and is expected to be folded into `aero-machine` as
  the canonical machine grows to include more devices (PCI, timers, interrupts, etc.).

#### `crates/emulator` (`emulator`)

**What it does**
- The current "device + I/O stack" crate: PCI, VGA/VBE, USB, storage backends, networking, etc.

**How it fits**
- Not the canonical VM *wiring* crate (it does not define the stable `Machine` API that `aero-wasm` consumes).
- Provides richer device stacks and conformance harnesses that will eventually be integrated with `aero-machine`.
- Some low-level, cross-runtime primitives are intentionally factored out of `emulator` into small shared crates.
  For networking, the minimal `NetworkBackend` trait plus the L2 tunnel backends (`L2TunnelBackend`,
  `L2TunnelRingBackend`) live in `crates/aero-net-backend` and are re-exported by `emulator::io::net`
  for compatibility.

### Legacy / prototypes (excluded from workspace)

These were valuable stepping stones, but they are **not** used by production wiring anymore and are kept under `crates/legacy/` for reference.

#### `crates/legacy/vm` (`vm`) — legacy

- Historical "Minimal VM wiring for the BIOS firmware tests".
- Superseded by `crates/aero-machine`.

#### `crates/legacy/aero-emulator` (`aero-emulator`) — legacy

- Prototype emulator implementation (VBE/VGA/AeroGPU experiments).
- Superseded by the current `crates/emulator` device stack + the canonical `crates/aero-machine` wiring crate.

### `crates/legacy/aero-vm` (`aero-vm`) — legacy demo VM (excluded from workspace)

- A deterministic toy VM used by snapshot demo panels.
- Marked `#[deprecated]` in favor of `aero_machine::Machine`.
- Archived under `crates/legacy/` once `crates/aero-wasm::DemoVm` switched to wrapping the canonical
  `aero-machine` implementation.
