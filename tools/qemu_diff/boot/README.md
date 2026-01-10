# QEMU differential boot sector

`boot.bin` is a **512-byte** 16-bit real-mode boot sector used by the `qemu_diff` Rust crate.

The repo commits `boot.bin` so the harness does **not** need an assembler in CI.

## Regenerating `boot.bin`

On a Linux host with `as`, `ld`, and `objcopy` (binutils):

```bash
cd tools/qemu_diff/boot
as --32 -o boot.o boot.S
ld -m elf_i386 -Ttext 0x7c00 -o boot.elf boot.o
objcopy -O binary boot.elf boot.bin
```

The boot sector prints a single `AERODIFF ...` line to QEMU's `isa-debugcon` (port `0xE9`), then exits QEMU via `isa-debug-exit` (port `0xF4`).

