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

The BIOS uses a host-side [`BlockDevice`] (512-byte sectors) as its boot medium during POST
(MBR/boot-sector reads and El Torito catalog scanning).

For BIOS INT 13h, the firmware also supports an optional separate CD-ROM backend:

- [`CdromDevice`] (2048-byte sectors), used for CD drive numbers (`DL=0xE0..=0xEF`).

To boot from different kinds of media, configure [`BiosConfig::boot_drive`] and provide the
corresponding bytes through that [`BlockDevice`]:

- **HDD/floppy (MBR/boot sector):** pass a disk image as a 512-byte-sector [`BlockDevice`] and set
  `boot_drive` to a floppy number (`0x00..=0x7F`) or HDD number (`0x80..=0xDF`).
- **CD-ROM / ISO (El Torito, no-emulation):** pass the **raw ISO bytes** through the same
  512-byte-sector [`BlockDevice`] and set `boot_drive` to a CD drive number (`0xE0..=0xEF`).
  Internally, the BIOS reads 2048-byte ISO logical blocks by issuing four 512-byte `read_sector`
  calls.

If your VM/storage stack already represents ISO media as **2048-byte sectors** (common for ATAPI /
MMC CD-ROM models), you still need to expose it to `firmware::bios` as a 512-byte-sector
[`BlockDevice`]. The required mapping is:

- `lba2048 = lba512 / 4`
- `sub_offset = (lba512 % 4) * 512`

…then read the containing 2048-byte block and copy out the requested 512-byte slice.

For ISO media, [`BlockDevice::size_in_sectors`] must return the size in **512-byte** units, i.e.
`iso_sector_count_2048 * 4`.

Example adapter (pseudocode) from a 2048-byte ISO backend into `firmware::bios::BlockDevice`:

```rust
/// 2048-byte-sector, read-only ISO/CD backend.
trait Iso2048 {
    fn read_block(&mut self, lba2048: u32, buf: &mut [u8; 2048]) -> Result<(), ()>;
    fn block_count(&self) -> u32;
}

/// Expose an ISO backend to `firmware::bios` by translating 512-byte sector reads into
/// 2048-byte ISO logical blocks.
struct IsoAsBlockDevice<I> {
    iso: I,
}

impl<I: Iso2048> firmware::bios::BlockDevice for IsoAsBlockDevice<I> {
    fn read_sector(&mut self, lba512: u64, out: &mut [u8; firmware::bios::BIOS_SECTOR_SIZE]) -> Result<(), firmware::bios::DiskError> {
        let lba512 = u32::try_from(lba512).map_err(|_| firmware::bios::DiskError::OutOfRange)?;
        let lba2048 = lba512 / 4;
        let sub = (lba512 % 4) as usize * firmware::bios::BIOS_SECTOR_SIZE;
        if lba2048 >= self.iso.block_count() {
            return Err(firmware::bios::DiskError::OutOfRange);
        }
        let mut block = [0u8; 2048];
        self.iso
            .read_block(lba2048, &mut block)
            .map_err(|_| firmware::bios::DiskError::OutOfRange)?;
        out.copy_from_slice(&block[sub..sub + firmware::bios::BIOS_SECTOR_SIZE]);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        u64::from(self.iso.block_count()) * 4
    }
}
```

The BIOS uses the following **drive numbers** in `DL`:

- First HDD: `DL = 0x80`
- First CD-ROM: `DL = 0xE0` (range `0xE0..=0xEF`)

> Note: The configured [`BiosConfig::boot_drive`] determines which device’s boot path is executed
> during POST (HDD/MBR vs El Torito) and what value is passed to the boot image in `DL`.
>
> When the optional "CD-first when present" policy is enabled (`BiosConfig::boot_from_cd_if_present`),
> POST may *temporarily* boot from CD (setting `DL=0xE0`) while keeping `BiosConfig::boot_drive` as the
> HDD fallback. Use [`Bios::booted_from_cdrom()`] to query what firmware actually booted from for the
> current session.
>
> **Drive presence (INT 13h):**
>
> - For HDD drive numbers (`0x80..=0xDF`), presence is derived from the BDA “fixed disk count”
>   field (0x40:0x75). [`ivt::init_bda`] seeds this based on the configured `boot_drive`, but
>   integrations that expose an HDD alongside a CD-ROM (for example, by passing a `cdrom` backend to
>   POST/interrupt dispatch) patch the BDA so **HDD0 remains present even when booting from CD**.
> - For CD drive numbers (`0xE0..=0xEF`), presence is `cdrom_present || drive == boot_drive`:
>   - If a [`CdromDevice`] is supplied to [`Bios::dispatch_interrupt`], the BIOS treats CD drive
>     numbers as present and services them using 2048-byte sectors directly.
>   - In legacy "ISO as BlockDevice" wiring (no `CdromDevice`), the configured `boot_drive` is still
>     treated as present so El Torito boot images can access the ISO via INT 13h Extensions.
>
> In minimal wiring where only a single `disk: &mut dyn BlockDevice` is provided (no `CdromDevice`
> and no BDA patching), this effectively behaves like a **single boot device** model: CD drive
> numbers are only present when `boot_drive` selects a CD (`0xE0..=0xEF`), and HDD drive numbers are
> only present when the BDA fixed-disk count is non-zero (typically when `boot_drive` selects an
> HDD).
>
> In the canonical full-system machine stack, both **HDD0 (`DL=0x80`) and CD0 (`DL=0xE0`) can be
> present at the same time**, which is required for Windows install media (boot from CD while still
> allowing the installer to access the HDD via INT 13h).
>
> INT 13h note: for CD drive numbers, the BIOS implements **INT 13h Extensions** (at minimum
> AH=41h/42h/48h) and treats the DAP `LBA` and `count` fields as **2048-byte logical blocks**
> (ISO LBAs), with `AH=48h` reporting `bytes_per_sector = 2048`.
>
> Backend note: for CD drive numbers, INT 13h can be backed either by:
>
> - a 2048-byte-sector [`CdromDevice`] (preferred), or
> - a legacy fallback where the raw ISO bytes are exposed via the 512-byte-sector [`BlockDevice`],
>   and the BIOS converts `lba2048 -> lba512 = lba2048 * 4`.
>
> Classic CHS INT 13h functions are not supported for CD drives; boot images are expected to use
> the extensions path.
>
> When booting via El Torito, the BIOS also captures boot-catalog metadata during POST and exposes
> it via **INT 13h AH=4Bh** ("El Torito disk emulation services"), which some CD boot images use.
>
> For the detailed El Torito + INT 13h Extensions behavior expected by Windows install media, see
> [`docs/09b-eltorito-cd-boot.md`](../../docs/09b-eltorito-cd-boot.md).

#### Firmware lifecycle wiring

- On reset:
  - Choose a boot drive number via [`BiosConfig::boot_drive`].
  - Pass the boot medium as the `disk: &mut dyn BlockDevice` argument to POST:
    - When booting from `DL=0x80`, `disk` should be the HDD image.
    - When booting from `DL=0xE0`, `disk` should expose the raw ISO bytes (so El Torito catalog
      scanning can read ISO blocks via `read_sector`).
  - Call [`Bios::post`] (or [`Bios::post_with_pci`] if you want PCI IRQ routing).
  - Resume execution at the CPU state configured by POST:
    - MBR boot: `CS:IP = 0000:7C00`
    - El Torito no-emulation: `CS:IP = <boot_catalog_load_segment>:0000` (commonly `07C0:0000`,
      i.e. physical `0x7C00`)
- On CPU exit:
  - If the CPU exits due to a BIOS stub `HLT`, call [`Bios::dispatch_interrupt`].
    - For INT 13h, you may additionally provide a `cdrom: Option<&mut dyn CdromDevice>` so the BIOS
      can service `DL=0xE0..=0xEF` requests using 2048-byte sectors directly.
- Keyboard input:
  - Push keys into the BIOS buffer via [`Bios::push_key`] (`(scan_code << 8) | ascii`).

### Boot drive numbering (DL) + CD-ROM/ISO boot (El Torito)

The BIOS boot drive number is configured via [`BiosConfig::boot_drive`] and is placed in `DL` when
firmware transfers control to the bootloader (both during POST and via INT 19h).

Boot drive numbering follows the conventional PC/BIOS ranges:

- `DL=0x80` — first fixed disk (**HDD0**)
- `DL=0xE0` — first CD-ROM drive (**CD0**; El Torito)

#### CD boot expectations (no-emulation + EDD)

For CD/ISO boots Aero expects an **El Torito _no-emulation_** boot entry. In no-emulation mode, the
boot image is expected to use **INT 13h Extensions / EDD** rather than CHS reads.

When `DL` is in the CD-ROM range (`0xE0..=0xEF`), Aero’s INT 13h implementation supports:

- `AH=41h` — check extensions present
- `AH=42h` — extended read (Disk Address Packet)
- `AH=48h` — extended get drive parameters (must report `bytes_per_sector = 2048`)

For CD drives, these EDD functions operate in **2048-byte logical blocks** (i.e. the DAP `lba` and
`count` are in units of 2048-byte sectors, not 512-byte sectors). Internally, the firmware-side
[`BlockDevice`] interface remains 512-byte based; the BIOS translates by mapping ISO LBA `n` to
`BlockDevice` LBA `n * 4`.

CHS functions like `AH=02h` (read sectors) are 512-byte/CHS-oriented and are not expected to work
for CD drives in Aero BIOS; the EDD path is the compatibility surface.

#### VM integration note (wiring a disk vs an ISO)

[`Bios::post`] takes a `&mut dyn BlockDevice` for the selected boot medium. [`Bios::dispatch_interrupt`]
takes that same `BlockDevice` plus an optional `cdrom: Option<&mut dyn CdromDevice>` for servicing
CD drive numbers (`DL=0xE0..`).

- If booting from HDD (`boot_drive = 0x80`), pass the HDD backend (512-byte sectors).
- If booting from CD (`boot_drive = 0xE0`), pass the ISO backend and expose CD semantics via EDD
  (`AH=41/42/48`, `bytes_per_sector=2048`). The ISO backend should be exposed as **raw ISO bytes**
  addressable in 512-byte chunks (i.e. `read_sector(lba)` reads at `lba * 512`); BIOS handles the
  2048-byte sector translation.

For the canonical Windows 7 topology (AHCI HDD + IDE/ATAPI CD-ROM) and the CD-first boot/install
flow, see [`docs/05-storage-topology-win7.md`](../../docs/05-storage-topology-win7.md).
For more BIOS/INT 13h background, see [`docs/09-bios-firmware.md`](../../docs/09-bios-firmware.md).

In the canonical full-system VM stack, [`aero_machine::Machine`](../aero-machine/src/lib.rs)
handles this automatically by dispatching BIOS interrupts when Tier-0 reports a `BiosInterrupt`
exit.

For the canonical machine wiring / integration reference, see `crates/aero-machine`
([`aero_machine::Machine`](../aero-machine/src/lib.rs); see
[ADR 0014](../../docs/adr/0014-canonical-machine-stack.md)).

`crates/legacy/vm` (formerly `crates/vm`) remains as a deprecated, reference-only VM stack and is
**not built by default** (it is excluded from the workspace).

## ACPI DSDT fixtures (`crates/firmware/acpi/dsdt*.aml`)

This repo keeps checked-in DSDT AML blobs under `crates/firmware/acpi/` for:

- ACPICA `iasl` decompile/recompile validation in CI (`scripts/validate-acpi.sh`)
- quick manual inspection / diffing

The fixtures are:

- `crates/firmware/acpi/dsdt.aml` — legacy PCI root bridge (ECAM/MMCONFIG disabled)
- `crates/firmware/acpi/dsdt_pcie.aml` — PCIe root bridge + ECAM/MMCONFIG enabled (Win7-relevant)

Clean-room, human-readable references live alongside as ASL sources:

- `crates/firmware/acpi/dsdt.asl`
- `crates/firmware/acpi/dsdt_pcie.asl`

The canonical source of truth is the `aero-acpi` Rust generator. To regenerate the checked-in
fixtures after AML changes:

```bash
cargo xtask fixtures
```

To verify the fixture is up to date without modifying it:

```bash
cargo xtask fixtures --check
```

Or, to check just the legacy `dsdt.aml` fixture:

```bash
cargo run -p firmware --bin gen_dsdt --locked -- --check
```

The DSDT fixtures are validated in CI in two ways:

- `scripts/verify_dsdt.sh` compiles the clean-room ASL sources and ensures they match the shipped AML fixtures.
- `scripts/validate-acpi.sh` decompiles and recompiles the checked-in AML tables using ACPICA `iasl`.
- `crates/firmware/tests/acpi_tables.rs` asserts that each checked-in DSDT fixture matches the bytes
  produced by the `aero-acpi` Rust generator.

For convenience, you can also regenerate just the legacy `dsdt.aml` fixture via:

```bash
cargo run -p firmware --bin gen_dsdt --locked
```

## BIOS ROM fixture (`assets/bios.bin`)

This repo keeps a checked-in BIOS ROM image at `assets/bios.bin` for:

- repository policy allowlisting of a tiny, deterministic firmware blob
- quick manual inspection / diffing

The canonical source of truth is the Rust generator (`firmware::bios::build_bios_rom()`).

To regenerate the checked-in fixture:

```bash
cargo xtask fixtures
```

To verify the fixture is up to date without modifying it:

```bash
cargo xtask fixtures --check
```

Preferred (regen/check BIOS ROM only):

```bash
cargo xtask bios-rom
cargo xtask bios-rom --check
```

Or, to regenerate/check just the BIOS ROM fixture via the standalone generator binary:

```bash
cargo run -p firmware --bin gen_bios_rom --locked
cargo run -p firmware --bin gen_bios_rom --locked -- --check
```

The `assets/bios.bin` fixture is validated by `crates/firmware/tests/bios_rom_fixture.rs`.
