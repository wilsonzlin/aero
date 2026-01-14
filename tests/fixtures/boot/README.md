# Boot / Disk Image Fixtures

This directory contains **tiny, deterministic, license-safe** fixtures for
system/integration tests (boot sectors + minimal disk images).

## Why fixtures live in-repo

We **do not** commit proprietary BIOS/OS images (Windows, vendor BIOS ROMs, etc).
Instead we commit:

- The **sources** for the fixtures (e.g. `boot_vga_serial.rs`)
- The **generated outputs** (e.g. `boot_vga_serial.bin`, `boot_vga_serial_8s.img`)

The generated binaries are intentionally kept very small (< 1MiB each; typically
only a few KiB) to keep the repo healthy.

## Regenerating fixtures

Run:

```bash
cargo xtask fixtures
```

CI runs `cargo xtask fixtures --check` and fails if any committed fixture output is
missing or out-of-date.

Note: `cargo xtask fixtures` also regenerates other tiny, deterministic firmware
fixtures outside this directory (e.g. `assets/bios.bin`, `crates/firmware/acpi/dsdt.aml`, `crates/firmware/acpi/dsdt_pcie.aml`).

## Fixture: `boot_vga_serial`

The `boot_vga_serial` boot sector:

- writes `AERO!` (attribute `0x1F`) to the start of the VGA text buffer at
  physical address `0xB8000`
- writes `AERO!\r\n` to COM1 (`0x3F8`) using `out dx, al`

## Fixture: `int_sanity`

The `int_sanity` boot sector is a tiny BIOS interrupt smoke test used by
unit/integration tests (e.g. `crates/legacy/vm/tests/boot_payloads.rs`). The
human-readable source lives in `int_sanity.asm`, but the committed
`int_sanity.bin` is generated/validated by `cargo xtask fixtures` so CI does not
require an assembler.

It exercises a small subset of BIOS interrupts (INT 10h/13h/15h/16h) and writes
observable results into low RAM so the host-side harness can assert behaviour.
