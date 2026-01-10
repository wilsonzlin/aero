//! Tiny 16-bit boot sector "program" used by system/integration tests.
//!
//! This file intentionally contains *only* source material (no binaries). The `xtask`
//! helper turns this into:
//! - `tests/fixtures/boot/boot_vga_serial.bin` (512-byte boot sector)
//! - `tests/fixtures/boot/boot_vga_serial_8s.img` (tiny disk image)
//!
//! The machine code is listed explicitly to keep generation deterministic and
//! license-safe (no external BIOS/OS images, no assembler toolchain required).

#![allow(dead_code)]

/// 16-bit machine code that the `xtask` pads to 510 bytes and appends the 0x55AA
/// boot signature.
///
/// Assembly (NASM-style) reference:
/// ```text
/// bits 16
/// org 0x7c00
/// start:
///   cli
///   xor ax, ax
///   mov ds, ax
///   mov es, ax
///   mov ss, ax
///   mov sp, 0x7c00
///   cld
///
///   mov ax, 0xb800
///   mov es, ax
///   xor di, di
///
///   mov ax, 0x1f41  ; 'A'
///   stosw
///   mov ax, 0x1f45  ; 'E'
///   stosw
///   mov ax, 0x1f52  ; 'R'
///   stosw
///   mov ax, 0x1f4f  ; 'O'
///   stosw
///   mov ax, 0x1f21  ; '!'
///   stosw
///
///   mov dx, 0x3f8   ; COM1
///   mov al, 'A'     ; send "AERO!\r\n"
///   out dx, al
///   mov al, 'E'
///   out dx, al
///   mov al, 'R'
///   out dx, al
///   mov al, 'O'
///   out dx, al
///   mov al, '!'
///   out dx, al
///   mov al, 0x0d
///   out dx, al
///   mov al, 0x0a
///   out dx, al
///
/// hang:
///   jmp hang
/// ```
pub const CODE: &[u8] = &[
    0xFA, // cli
    0x31, 0xC0, // xor ax, ax
    0x8E, 0xD8, // mov ds, ax
    0x8E, 0xC0, // mov es, ax
    0x8E, 0xD0, // mov ss, ax
    0xBC, 0x00, 0x7C, // mov sp, 0x7c00
    0xFC, // cld
    0xB8, 0x00, 0xB8, // mov ax, 0xb800
    0x8E, 0xC0, // mov es, ax
    0x31, 0xFF, // xor di, di
    0xB8, 0x41, 0x1F, // mov ax, 0x1f41 ('A' + attribute)
    0xAB, // stosw
    0xB8, 0x45, 0x1F, // mov ax, 0x1f45 ('E' + attribute)
    0xAB, // stosw
    0xB8, 0x52, 0x1F, // mov ax, 0x1f52 ('R' + attribute)
    0xAB, // stosw
    0xB8, 0x4F, 0x1F, // mov ax, 0x1f4f ('O' + attribute)
    0xAB, // stosw
    0xB8, 0x21, 0x1F, // mov ax, 0x1f21 ('!' + attribute)
    0xAB, // stosw
    0xBA, 0xF8, 0x03, // mov dx, 0x3f8 (COM1)
    0xB0, 0x41, // mov al, 'A'
    0xEE, // out dx, al
    0xB0, 0x45, // mov al, 'E'
    0xEE, // out dx, al
    0xB0, 0x52, // mov al, 'R'
    0xEE, // out dx, al
    0xB0, 0x4F, // mov al, 'O'
    0xEE, // out dx, al
    0xB0, 0x21, // mov al, '!'
    0xEE, // out dx, al
    0xB0, 0x0D, // mov al, '\r'
    0xEE, // out dx, al
    0xB0, 0x0A, // mov al, '\n'
    0xEE, // out dx, al
    0xEB, 0xFE, // jmp $
];

/// Expected `VGA` text buffer bytes at `0xB8000` after executing the boot sector.
/// Each character cell is 2 bytes: ASCII + attribute.
pub const EXPECTED_VGA_TEXT_BYTES: [u8; 10] = [
    b'A', 0x1F, // A
    b'E', 0x1F, // E
    b'R', 0x1F, // R
    b'O', 0x1F, // O
    b'!', 0x1F, // !
];

/// Expected raw serial output bytes written to COM1 (0x3F8).
pub const EXPECTED_SERIAL_BYTES: [u8; 7] = [b'A', b'E', b'R', b'O', b'!', b'\r', b'\n'];
