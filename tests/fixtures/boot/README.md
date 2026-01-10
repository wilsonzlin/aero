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

CI re-runs this command and fails if it produces any diff (determinism check).

## Fixture: `boot_vga_serial`

The `boot_vga_serial` boot sector:

- writes `AERO!` (attribute `0x1F`) to the start of the VGA text buffer at
  physical address `0xB8000`
- writes `AERO!\r\n` to COM1 (`0x3F8`) using `out dx, al`

