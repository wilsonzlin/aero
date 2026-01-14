# `aero-smp-model` crate

This crate contains a **minimal, deterministic SMP/APIC model** used for unit tests and snapshot
validation.

It intentionally models only a small subset of x86 SMP bring-up:

- Per-vCPU run state (BSP reset, AP wait-for-SIPI)
- Local APIC IPI delivery (INIT/SIPI/fixed)
- A deterministic round-robin scheduler
- A snapshot adapter (`aero-snapshot` integration) used by tests

This is **not** the canonical full-system VM wiring layer. The canonical VM lives in:

- `crates/aero-machine` (`aero_machine::Machine`)

## Key types

- `aero_smp_model::SmpMachine` — minimal multi-vCPU machine state (CPU+LAPIC+RAM)
- `aero_smp_model::DeterministicScheduler` — deterministic scheduling harness for tests

## Relationship to `crates/emulator`

`crates/emulator` intentionally does **not** expose this model by default.

For backwards compatibility, it can be accessed via `emulator::smp` when enabling:

```bash
cargo test -p emulator --features legacy-smp-model
```

