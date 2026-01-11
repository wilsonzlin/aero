# ADR 0014: Canonical machine/VM stack (`aero-machine`)

## Context

The repository historically accumulated multiple “VM” stacks:

- `crates/emulator`: full-system-ish device models + chipset glue (bespoke buses).
- `crates/legacy/vm` (formerly `crates/vm`): a toy real-mode VM used for early firmware tests.
- `crates/legacy/aero-vm` (formerly `crates/aero-vm`): a deterministic stub VM used during early snapshot demo bring-up.
- `crates/aero-wasm`: exported the stub `DemoVm`, leaving it unclear which machine is the
  intended browser runtime.

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
  which VM runs in-browser.
- The following crates are considered **deprecated** and should not be used for new work:
  - `crates/legacy/aero-vm` (stub VM)
  - `crates/legacy/vm` (toy BIOS VM)
  - (removed) `crates/machine` (toy real-mode CPU/memory primitives)
  - Emulator-internal duplicate device models/buses (migration TBD)

### Migration plan (incremental)

1. Introduce `crates/aero-machine` and add a minimal end-to-end boot test (serial + MBR).
2. Switch `crates/aero-wasm` (and web examples) to use `aero_machine::Machine`.
3. Add deprecation markers to the old VM crates and remove remaining dependencies on them.
4. Follow-up: refactor `crates/emulator` to consume `aero-machine`/`aero-devices` wiring (or move
   remaining unique functionality behind shared interfaces).
