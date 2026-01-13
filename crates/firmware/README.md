# `firmware`

This crate contains clean-room firmware components used by the Aero emulator.

## Legacy BIOS HLE (`firmware::bios`)

`firmware::bios` is the **canonical legacy BIOS implementation** used by the VM core. It provides:

- `build_bios_rom()` → a 64KiB BIOS ROM image containing interrupt stubs.
- A host-side [`Bios`] implementation for POST and a minimal INT service surface
  (INT 10h/11h/12h/13h/14h/15h/16h/17h/18h/19h/1Ah, plus ACPI + SMBIOS publication, and El Torito
  CD-ROM boot + INT 13h CD reads when a [`CdromDevice`] is supplied).

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
5. On that exit the host calls the BIOS interrupt dispatcher with the vector number
   ([`Bios::dispatch_interrupt`] for HDD-only wiring, or [`Bios::dispatch_interrupt_with_cdrom`] when
   a CD-ROM device is also wired),
   then resumes the CPU. The next instruction is `IRET`, which returns to the
   original caller.

This keeps the CPU implementation generic (important for a future JIT) while still
allowing BIOS services to live in Rust.

### VM integration checklist

#### Storage devices (multi-boot: HDD/floppy + CD-ROM)

The BIOS can expose (and boot from) multiple INT 13h devices:

- **HDD/floppy**: a 512-byte-sector [`BlockDevice`] (read-only) used for legacy INT 13h reads and
  MBR/boot-sector boot.
- **CD-ROM / ISO**: a 2048-byte-sector [`CdromDevice`] (read-only) used for **El Torito** boot and
  for INT 13h CD reads (typically via INT 13h Extensions).

The BIOS uses the following **drive numbers** in `DL`:

- First HDD: `DL = 0x80`
- First CD-ROM: `DL = 0xE0`

> Note: The configured [`BiosConfig::boot_drive`] determines which device’s boot path is executed
> during POST (HDD/MBR vs El Torito) and what value is passed to the boot image in `DL`.

#### Firmware lifecycle wiring

- On reset:
  - Call the appropriate BIOS POST entrypoint:
    - **HDD-only (legacy convenience wrappers):** [`Bios::post`] / [`Bios::post_with_pci`]
    - **HDD + CD-ROM (El Torito + INT 13h CD reads):** [`Bios::post_with_cdrom`] (or
      [`Bios::post_with_pci_and_cdrom`] if you also want PCI enumeration).
  - Begin execution at the boot image/sector (`CS:IP = 0000:7C00`).
- On CPU exit:
  - If the CPU exits due to a BIOS stub `HLT`, call the corresponding interrupt dispatcher:
    - **HDD-only (legacy convenience wrapper):** [`Bios::dispatch_interrupt`]
    - **HDD + CD-ROM:** [`Bios::dispatch_interrupt_with_cdrom`]
  - The HDD-only wrappers remain for call sites that only wire a [`BlockDevice`]; they do **not**
    provide CD-ROM services.
- Keyboard input:
  - Push keys into the BIOS buffer via [`Bios::push_key`] (`(scan_code << 8) | ascii`).

In the canonical full-system VM stack, [`aero_machine::Machine`](../aero-machine/src/lib.rs)
handles this automatically by dispatching BIOS interrupts when Tier-0 reports a `BiosInterrupt`
exit.

For the canonical machine wiring / integration reference, see `crates/aero-machine`
([`aero_machine::Machine`](../aero-machine/src/lib.rs); see
[ADR 0014](../../docs/adr/0014-canonical-machine-stack.md)).

`crates/legacy/vm` (formerly `crates/vm`) remains as a deprecated, reference-only VM stack and is
**not built by default** (it is excluded from the workspace).

## ACPI DSDT fixture (`crates/firmware/acpi/dsdt.aml`)

This repo keeps a checked-in DSDT AML blob at `crates/firmware/acpi/dsdt.aml` for:

- ACPICA `iasl` decompile/recompile validation in CI (`scripts/validate-acpi.sh`)
- quick manual inspection / diffing

The canonical source of truth is the `aero-acpi` Rust generator. To regenerate the checked-in
fixture after AML changes:

```bash
cargo xtask fixtures
```

To verify the fixture is up to date without modifying it:

```bash
cargo run -p firmware --bin gen_dsdt --locked -- --check
```

The `dsdt.aml` fixture is validated in CI in two ways:

- `scripts/validate-acpi.sh` decompiles and recompiles the checked-in AML tables using ACPICA `iasl`.
- `crates/firmware/tests/acpi_tables.rs` asserts that `crates/firmware/acpi/dsdt.aml` matches the
  bytes produced by the `aero-acpi` Rust generator.

For convenience, you can also regenerate just the DSDT fixture via:

```bash
cargo run -p firmware --bin gen_dsdt --locked
```
