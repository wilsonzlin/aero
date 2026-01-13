# `firmware`

This crate contains clean-room firmware components used by the Aero emulator.

## Legacy BIOS HLE (`firmware::bios`)

`firmware::bios` is the **canonical legacy BIOS implementation** used by the VM core. It provides:

- `build_bios_rom()` → a 64KiB BIOS ROM image containing interrupt stubs.
- A host-side [`Bios`] implementation for POST and a minimal INT service surface
  (INT 10h/11h/12h/13h/14h/15h/16h/17h/18h/19h/1Ah, plus ACPI + SMBIOS publication, and El Torito
  no-emulation CD-ROM boot + INT 13h Extensions reads when `BiosConfig::boot_drive` is a CD drive).

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

#### Boot device wiring (HDD/floppy vs CD-ROM/ISO)

The BIOS uses a single host-side [`BlockDevice`] (512-byte sectors) as its boot medium.

To boot from different kinds of media, configure [`BiosConfig::boot_drive`] and provide the
corresponding bytes through that [`BlockDevice`]:

- **HDD/floppy (MBR/boot sector):** pass a disk image as a 512-byte-sector [`BlockDevice`] and set
  `boot_drive` to a floppy number (`0x00..=0x7F`) or HDD number (`0x80..=0xDF`).
- **CD-ROM / ISO (El Torito, no-emulation):** pass the **raw ISO bytes** through the same
  512-byte-sector [`BlockDevice`] and set `boot_drive` to a CD drive number (`0xE0..=0xEF`).
  Internally, the BIOS reads 2048-byte ISO logical blocks by issuing four 512-byte `read_sector`
  calls.

The BIOS uses the following **drive numbers** in `DL`:

- First HDD: `DL = 0x80`
- First CD-ROM: `DL = 0xE0` (range `0xE0..=0xEF`)

> Note: The configured [`BiosConfig::boot_drive`] determines which device’s boot path is executed
> during POST (HDD/MBR vs El Torito) and what value is passed to the boot image in `DL`.
>
> Also note that this BIOS currently models **exactly one boot device** (backed by the single
> [`BlockDevice`] passed to POST/interrupt handlers). In particular, when booting from a CD-ROM
> drive number, the BIOS reports no fixed disks in the BDA (see `bios::ivt::init_bda`).
>
> INT 13h note: for CD drive numbers, the BIOS implements **INT 13h Extensions** (at minimum
> AH=41h/42h/48h) and treats the DAP `LBA` and `count` fields as **2048-byte logical blocks**
> (ISO LBAs), even though the host-side backing store is exposed as 512-byte sectors.

#### Firmware lifecycle wiring

- On reset:
  - Choose a boot drive number via [`BiosConfig::boot_drive`]. There are currently no separate
    CD-specific POST/dispatch entrypoints; CD boot/reads are selected purely by the `DL` drive
    number.
  - Call [`Bios::post`] (or [`Bios::post_with_pci`] if you want PCI IRQ routing).
  - Resume execution at the CPU state configured by POST:
    - MBR boot: `CS:IP = 0000:7C00`
    - El Torito no-emulation: `CS:IP = <boot_catalog_load_segment>:0000` (commonly `07C0:0000`,
      i.e. physical `0x7C00`)
- On CPU exit:
  - If the CPU exits due to a BIOS stub `HLT`, call [`Bios::dispatch_interrupt`].
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
cargo xtask fixtures --check
```

Or, to check just the DSDT fixture:

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
