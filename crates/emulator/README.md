# `emulator` crate (legacy/compat device stack)

`crates/emulator` is **not** the canonical “VM wiring” layer.

The canonical full-system machine integration layer is:

- `crates/aero-machine` (`aero_machine::Machine`)

See:

- [`docs/vm-crate-map.md`](../../docs/vm-crate-map.md) (canonical vs legacy crate map)
- [ADR 0008: canonical VM core](../../docs/adr/0008-canonical-vm-core.md)
- [ADR 0014: canonical machine stack](../../docs/adr/0014-canonical-machine-stack.md)
- [`docs/21-emulator-crate-migration.md`](../../docs/21-emulator-crate-migration.md) (explicit migration plan + deletion targets)

## What this crate is for (today)

This crate currently exists primarily as:

- A **compatibility shim** for older `emulator::...` import paths (re-exporting canonical crates), and
- A home for a few **remaining unique subsystems** that have not yet been extracted into the
  canonical `aero-machine`/`aero-devices` stack.

## What should *not* land here

Do not add new “canonical” work to this crate:

- New machine wiring / platform composition belongs in `crates/aero-machine` / `crates/aero-pc-platform`.
- New VGA/VBE work belongs in `crates/aero-gpu-vga`.
- New USB device model work belongs in `crates/aero-usb` (see ADR 0015).
- New storage traits / disk formats belong in `crates/aero-storage` (see `docs/20-storage-trait-consolidation.md`).
- New network backend/pumping work belongs in `crates/aero-net-backend` / `crates/aero-net-pump`.

If you’re tempted to add something here, it probably means the canonical stack is missing an API and
should be extended instead.

## Remaining unique pieces (tracked for extraction)

- **AeroGPU PCI device model**: `src/devices/pci/aerogpu.rs` (+ `src/devices/aerogpu_*.rs`)
- **GPU worker / command executor wiring**: `src/gpu_worker/*`
- **Legacy deterministic SMP/APIC model**: extracted into `crates/aero-smp` (and re-exported as
  `emulator::smp`).

These are explicitly tracked in the migration plan doc and should not quietly expand in scope.
