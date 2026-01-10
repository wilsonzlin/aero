# `aero-bios`

Clean-room legacy BIOS implementation for the Aero emulator.

## What this provides (today)

- A **64KiB BIOS ROM** image builder (`aero_bios::build_bios_rom`) with a valid reset vector at `F000:FFF0`.
- A **Rust firmware implementation** (`aero_bios::Bios`) that an emulator can call for:
  - POST: IVT + BDA init, VGA text mode init, simple banner, and boot sector load to `0x7C00`.
  - INT services (minimal subset):
    - **INT 10h**: text mode + teletype output (`AH=0Eh`) + basic cursor functions.
    - **INT 13h**: CHS reads (`AH=02h`) and EDD extensions (`AH=41h/42h/48h`) for LBA reads.
    - **INT 15h**: E820 memory map (`EAX=E820h, EDX='SMAP'`) + `AH=88h` extended memory size.
    - **INT 16h**: simple keyboard polling.
    - **INT 19h**: bootstrap loader (loads LBA0 to `0x7C00` and jumps).
- Optional PCI enumeration via the `PciConfigSpace` trait with deterministic IRQ routing.
- Optional ACPI table publication (RSDP/RSDT/XSDT/FADT/MADT/HPET/DSDT/FACS) via the `aero-acpi`
  crate, with the E820 map marking the blob as ACPI reclaimable memory (type 3).

This is intentionally *small and conservative*; it is a foundation for later UEFI work and deeper
hardware initialization.

## Clean-room sources

This crate does **not** incorporate any proprietary BIOS code.

Implementation is based on publicly available documentation:

- Ralf Brown's Interrupt List (reference for legacy INT APIs)
- OSDev Wiki pages for BIOS, E820 ("SMAP"), and EDD (INT 13h extensions)
- Intel Software Developer's Manual (x86 interrupt/real-mode behaviour)
