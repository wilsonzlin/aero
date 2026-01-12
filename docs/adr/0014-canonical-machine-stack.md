# ADR 0014: Canonical machine/VM stack (`aero-machine`)

## Context

The repository historically accumulated multiple “VM” stacks:

- `crates/emulator`: full-system-ish device models + chipset glue (bespoke buses).
- `crates/legacy/vm` (formerly `crates/vm`): a toy real-mode VM used for early firmware tests.
- `crates/legacy/aero-vm` (formerly `crates/aero-vm`): a deterministic stub VM used during early snapshot demo bring-up.
- `crates/aero-wasm`: historically exported only the stub `DemoVm`, leaving it unclear which VM
  was the intended browser runtime.

In addition, **`crates/aero-wasm` currently exposes multiple WASM-facing VM wrappers**, each
targeting a different integration strategy:

- `aero-wasm::WasmVm` and `aero-wasm::WasmTieredVm`
  - Purpose: a **CPU-only** stepping loop for the browser **CPU worker runtime**
    (`web/src/workers/cpu.worker.ts`).
  - Design: executes the x86 CPU core inside WASM, but forwards port I/O and MMIO back to JS via
    shims (`globalThis.__aero_io_port_*`, `globalThis.__aero_mmio_*`). (`WasmTieredVm` also calls
    out to JS for Tier-1 JIT blocks via `globalThis.__aero_jit_call`.)
  - Status: useful for CPU/JIT iteration, but **not** the canonical full-system VM wiring surface.

- `aero-wasm::Machine`
  - Purpose: the **canonical full-system machine** exported to JS, backed by
    `aero_machine::Machine` (`crates/aero-machine`).
  - Owns the machine's PCI/IO/MMIO wiring in Rust/WASM (including the PCI E1000 NIC model) and can
    attach the browser `NET_TX`/`NET_RX` AIPC rings as a network backend.
  - Used by: `web/src/main.ts` serial boot demo today.
  - Intended: future “main” web runtime once the worker runtime is migrated.

- `aero-wasm::PcMachine`
  - Purpose: experimental wasm-bindgen wrapper around `aero_machine::PcMachine`.
  - Status: intended for experiments/tests; it allocates its own guest RAM inside the wasm module
    (does **not** use the `guest_ram_layout` shared-memory contract used by the worker runtime).
  - Not currently used by the main web runtime.

This split-brain makes it hard to answer basic questions like:

- “Which machine runs in-browser?”
- “Which CPU core is canonical?”
- “Which device models should new work build on?”

## Decision

Establish a single canonical full-system “machine integration layer” crate:

- **Canonical CPU engine:** `crates/aero-cpu-core` (`aero-cpu-core` / `aero_cpu_core`)
  - Rationale: it is the canonical CPU state representation and Tier-0 interpreter used by the
    tiered runtime/JIT path; it is `wasm32`-friendly.

- **Canonical device model layer:** `crates/devices` (`aero-devices`) composed using
  `crates/platform` (`aero-platform`) buses (`IoPortBus`, chipset state, interrupt router, …).

- **Canonical firmware stack:** `crates/firmware` (`firmware::bios`)
  - The BIOS logic is in Rust (`Bios::post`, `Bios::dispatch_interrupt`) and the ROM image is a
    discoverable stub (`build_bios_rom`) that emulators map for guests.

- **Canonical machine glue crate:** `crates/aero-machine` (`aero-machine`)
  - Provides `aero_machine::Machine`, which composes:
    - `aero_cpu_core` + `memory::PhysicalMemoryBus`
    - `aero-platform` port I/O bus + chipset state (A20/reset)
    - `aero-devices` serial/i8042/reset-control devices
    - `firmware::bios` POST bootstrapping and trapped BIOS interrupt services
  - Intended to be used by both Rust tests and the WASM/browser bindings.

## Alternatives considered

1. **Refactor `crates/emulator` to be the canonical machine directly**
   - Pros: avoids introducing a new crate.
   - Cons: large surface area and legacy bus/device duplication makes this a heavy refactor.

2. **Keep the existing split stacks**
   - Pros: no immediate churn.
   - Cons: perpetuates ambiguity and duplicated implementations; blocks browser integration.

## Consequences

- `crates/aero-wasm` must export the canonical machine (`aero_machine::Machine`) so it is explicit
  which VM is canonical for in-browser full-system work.
- The following crates are considered **deprecated** and should not be used for new work:
  - `crates/legacy/aero-vm` (stub VM)
  - `crates/legacy/vm` (toy BIOS VM)
  - (removed) `crates/machine` (toy real-mode CPU/memory primitives)
  - Emulator-internal duplicate device models/buses (migration TBD)

### Migration plan (incremental)

1. Introduce `crates/aero-machine` and add a minimal end-to-end boot test (serial + MBR).
2. Export `aero-wasm::Machine` as the canonical browser-facing full-system VM API (done), and use it
   for standalone demos (`web/src/main.ts`).
3. Keep `aero-wasm::WasmVm` / `aero-wasm::WasmTieredVm` as **legacy CPU-worker-only** harnesses while
   the web runtime still relies on JS shims for I/O/MMIO.
4. Migrate the main worker runtime from `WasmVm`/`WasmTieredVm` → canonical `Machine` by moving the
   full-system port/MMIO/device stepping loop into `aero-machine` (running in WASM) and using the
   existing AIPC rings as the host/device boundary (disk, GPU, audio, networking).
5. Add deprecation markers to the old VM crates and remove remaining dependencies on them.
6. Follow-up: refactor `crates/emulator` to consume `aero-machine`/`aero-devices` wiring (or move
   remaining unique functionality behind shared interfaces).
