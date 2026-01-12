# ADR 0008: Canonical VM core crate (`aero-machine`)

> Note: The broader "canonical machine stack" decision is also captured in
> [ADR 0014](./0014-canonical-machine-stack.md). This ADR focuses on consolidating
> the historical VM crate split and defining the minimal stable API surface.

## Context

The repository historically contained multiple partially-overlapping "VM" / "emulator" implementations:

- `crates/emulator` — large device + I/O stack (PCI/VGA/VBE/USB/storage/net/etc).
- `crates/legacy/aero-emulator` (formerly `crates/aero-emulator`) — prototype emulator implementation (VBE/VGA/AeroGPU experiments).
- `crates/legacy/vm` (formerly `crates/vm`) — "Minimal VM wiring for the BIOS firmware tests" (machine + firmware + snapshot glue).
- `crates/legacy/aero-vm` (formerly `crates/aero-vm`) — toy snapshot VM used during early snapshot demo bring-up.
- `crates/aero-machine` — full-system machine wiring (CPU + memory + port I/O + firmware) introduced to make the canonical VM core explicit.

This fragmentation created architectural ambiguity:

- Browser/WASM code and host integration tests were not building on the same VM core.
- There was no stable, documented public API for "VM wiring".
- Snapshot work was duplicated across multiple ad-hoc harnesses.

## Decision

### 1) Canonical VM wiring crate

The **canonical VM wiring crate is `crates/aero-machine` (`aero-machine`)**.

All code that wants to *construct and run the Aero VM/machine* (including `crates/aero-wasm` exports and host integration tests) should depend on `aero-machine` and use `aero_machine::Machine`.

### 2) Stable public API surface (minimum)

`aero-machine` exposes a stable API that higher layers depend on:

- **Creation/config**
  - `MachineConfig`
  - `Machine::new(MachineConfig) -> Result<Machine, MachineError>`
- **Run/step**
  - `Machine::reset()`
  - `Machine::run_slice(max_insts: u64) -> RunExit`
- **Device attachment hooks**
  - Disk image: `Machine::set_disk_image(Vec<u8>)`
  - Serial drain: `Machine::take_serial_output()`
  - Serial stats: `Machine::serial_output_len()` / `Machine::serial_output_bytes()`
  - Input injection (legacy PS/2 via i8042):
    - Keyboard: `Machine::inject_browser_key(code, pressed)`
    - Mouse: `Machine::inject_mouse_motion(dx, dy, wheel)`
    - Mouse buttons: `Machine::inject_mouse_button(button: Ps2MouseButton, pressed)`
      - Convenience wrappers: `inject_mouse_left/right/middle`
- **Debug/testing helpers**
  - Read guest physical memory: `Machine::read_physical_u8/u16/bytes(...)`
- **Snapshots (via `aero-snapshot`)**
  - `Machine` integrates with `aero_snapshot::{save_snapshot, restore_snapshot_with_options}` via snapshot helper methods.

### 3) Legacy/prototype crates

The following crates are **not** canonical VM wiring. They are kept under `crates/legacy/` and are excluded from the workspace:

- `crates/legacy/vm` — superseded by `crates/aero-machine`
- `crates/legacy/aero-emulator` — superseded by `crates/emulator` (device stack) + `crates/aero-machine` (wiring)
- `crates/legacy/aero-vm` — superseded by `crates/aero-machine` (with `crates/aero-wasm::DemoVm` as a deprecated wrapper)

`crates/aero-wasm` retains a deprecated `DemoVm` export for the snapshot demo UI, but it is a thin wrapper around the canonical `aero_machine::Machine` (not a separate VM core).

`crates/emulator` remains in the workspace as the current device + I/O model crate. It is *not* the canonical "VM wiring" surface consumed by `aero-wasm`.

### 4) WASM/browser-facing VM wrappers (current state)

`crates/aero-wasm` exposes multiple wasm-bindgen entrypoints that are easy to confuse:

- **`Machine`** (canonical): a full-system VM wrapper around `aero_machine::Machine`. This is the
  intended target for new browser integration work (PCI/device wiring, networking, snapshots, …).
- **`WasmVm` / `WasmTieredVm`** (legacy CPU-worker runtime): CPU-only stepping loops used by
  `web/src/workers/cpu.worker.ts`. They execute CPU in WASM but forward port I/O + MMIO back to JS
  via shims (`globalThis.__aero_io_port_*`, `globalThis.__aero_mmio_*`), and `WasmTieredVm` also
  calls out to JS for Tier-1 JIT blocks (`globalThis.__aero_jit_call`).
- **`PcMachine`** (experimental): wasm-bindgen wrapper around `aero_machine::PcMachine` primarily
  intended for experiments/tests; it allocates its own guest RAM and does not use the worker
  runtime `guest_ram_layout` shared-memory contract.

See also: [`docs/vm-crate-map.md`](../vm-crate-map.md) and [ADR 0014](./0014-canonical-machine-stack.md).

## Alternatives considered

1. **Make `crates/emulator` the canonical VM wiring**
   - Rejected (for now): `crates/emulator` is a large device stack, but does not define a stable top-level `Machine` API that both the browser and host tests can depend on.

2. **Keep `aero-vm` as canonical**
   - Rejected: `aero-vm` is intentionally a toy snapshot/demo VM; it does not model the full device + firmware + port I/O stack.

3. **Keep all crates and defer the decision**
   - Rejected: ambiguity was already blocking integration work (web runtime, snapshots, conformance harnesses).

## Consequences

- `crates/aero-wasm` builds on `aero-machine` for the canonical full-system VM path.
- Host-side VM integration tests should build on `aero-machine`.
- Legacy crates remain available for reference under `crates/legacy/`, but are excluded from `cargo test --workspace`.
- Future VM integration work (CPU/JIT, device models, web runtime ABI, snapshots) should plug into `aero-machine` rather than creating new ad-hoc VM wrappers.

See also: [`docs/vm-crate-map.md`](../vm-crate-map.md).
