# QEMU differential boot sector

`boot.bin` is a **512-byte** 16-bit real-mode boot sector used by the `qemu-diff` Cargo package
(crate identifier `qemu_diff`).

The repo commits `boot.bin` so the harness does **not** need an assembler in CI.

CI enforces determinism for this file via:

```bash
cargo xtask fixtures --check
```

## Regenerating `boot.bin`

The simplest way (no assembler required):

```bash
cargo xtask fixtures
```

If you intentionally change the reference assembly (`boot.S`), rebuild `boot.bin`
on a Linux host with `as`, `ld`, and `objcopy` (binutils) and update the embedded
fixture bytes in `xtask/src/fixture_sources/qemu_diff_boot.rs` to match:

```bash
cd tools/qemu_diff/boot
as --32 -o boot.o boot.S
ld -m elf_i386 -Ttext 0x7c00 -o boot.elf boot.o
objcopy -O binary boot.elf boot.bin
```

The boot sector prints a single `AERODIFF ...` line to QEMU's `isa-debugcon` (port `0xE9`), then exits QEMU via `isa-debug-exit` (port `0xF4`).
