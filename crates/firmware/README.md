# `firmware`

This crate contains clean-room firmware components used by the Aero emulator.

## Legacy BIOS HLE (`firmware::bios`)

`firmware::bios` is the **canonical legacy BIOS implementation** used by the VM core. It provides:

- `build_bios_rom()` → a 64KiB BIOS ROM image containing interrupt stubs.
- A host-side [`Bios`] implementation for POST and a minimal INT service surface
  (INT 10h/11h/12h/13h/14h/15h/16h/18h/19h/1Ah, plus ACPI + SMBIOS publication).

### Dispatch contract (HLT-in-ROM-stub hypercall)

The BIOS is dispatched without trapping `INT xx` in the CPU core:

1. The VM maps the ROM from `build_bios_rom()` at [`BIOS_BASE`] with size [`BIOS_SIZE`].
   The conventional real-mode reset vector (`F000:FFF0`) corresponds to [`RESET_VECTOR_PHYS`].
   If the CPU models the architectural reset alias at the top of 4GiB, also map/alias the ROM at
   [`BIOS_ALIAS_BASE`] so the reset vector at [`RESET_VECTOR_ALIAS_PHYS`] is valid.
2. The CPU executes `INT imm8` normally in real mode (push FLAGS/CS/IP, clear IF/TF,
   and load CS:IP from the IVT).
3. During POST the BIOS writes the IVT to point at tiny ROM stubs (one per INT).
   Each stub is:

   ```text
   HLT
   IRET
   ```

4. The canonical CPU core (`aero_cpu_core`) treats `HLT` as a VM-exit **only when**
   it is reached from an `INT` stub. Tier-0 surfaces this as a `BiosInterrupt`
   exit that includes the vector number (recorded by the core when `INT imm8` is
   executed).
5. On that exit the host calls [`Bios::dispatch_interrupt`] with the vector number,
   then resumes the CPU. The next instruction is `IRET`, which returns to the
   original caller.

This keeps the CPU implementation generic (important for a future JIT) while still
allowing BIOS services to live in Rust.

### VM integration checklist

- On reset:
  - Call [`Bios::post`] (or [`Bios::post_with_pci`] if you want PCI IRQ routing).
  - Begin execution at the boot sector (`CS:IP = 0000:7C00`).
- On CPU exit:
  - If the CPU exits due to a BIOS stub `HLT`, call [`Bios::dispatch_interrupt`].
- Keyboard input:
  - Push keys into the BIOS buffer via [`Bios::push_key`] (`(scan_code << 8) | ascii`).

In the canonical full-system VM stack, [`aero_machine::Machine`](../aero-machine/src/lib.rs)
handles this automatically by dispatching BIOS interrupts when Tier-0 reports a `BiosInterrupt`
exit.

For a minimal, older reference wiring, see `crates/legacy/vm` (formerly `crates/vm`).

## ACPI DSDT fixture (`crates/firmware/acpi/dsdt.aml`)

This repo keeps a checked-in DSDT AML blob at `crates/firmware/acpi/dsdt.aml` for:

- ACPICA `iasl` decompile/recompile validation in CI (`scripts/validate-acpi.sh`)
- quick manual inspection / diffing

The canonical source of truth is the `aero-acpi` Rust generator. To regenerate the
fixture after AML changes:

```bash
cargo run -p firmware --bin gen_dsdt --locked
```
