#![cfg(target_arch = "wasm32")]

use aero_gpu_vga::{VBE_DISPI_DATA_PORT, VBE_DISPI_INDEX_PORT};
use aero_wasm::{Machine, RunExitKind};
use wasm_bindgen_test::wasm_bindgen_test;

const _: () = {
    assert!(VBE_DISPI_DATA_PORT == VBE_DISPI_INDEX_PORT + 1);
};

// Keep constants in sync with:
// - `crates/aero-shared/src/scanout_state.rs`
// - `web/src/ipc/scanout_state.ts`
const SCANOUT_STATE_U32_LEN: usize = 8;
const SCANOUT_STATE_BYTE_LEN: u32 = (SCANOUT_STATE_U32_LEN as u32) * 4;
const SCANOUT_STATE_GENERATION_BUSY_BIT: u32 = 1 << 31;

// Keep constants in sync with:
// - `crates/aero-shared/src/cursor_state.rs`
// - `web/src/ipc/cursor_state.ts`
#[cfg(feature = "wasm-threaded")]
const CURSOR_STATE_U32_LEN: usize = 12;
#[cfg(feature = "wasm-threaded")]
const CURSOR_STATE_BYTE_LEN: u32 = (CURSOR_STATE_U32_LEN as u32) * 4;

const SCANOUT_SOURCE_LEGACY_TEXT: u32 = 0;
const SCANOUT_SOURCE_LEGACY_VBE_LFB: u32 = 1;
// `ScanoutState.format` uses AeroGPU `AerogpuFormat` discriminants (`0` is `Invalid`).
const SCANOUT_FORMAT_B8G8R8X8: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScanoutSnapshot {
    generation: u32,
    source: u32,
    base_paddr: u64,
    width: u32,
    height: u32,
    pitch_bytes: u32,
    format: u32,
}

fn copy_linear_bytes(ptr: u32, len: u32) -> Vec<u8> {
    let mut out = vec![0u8; len as usize];
    // Safety: callers ensure ptr/len refer to a valid range in wasm linear memory.
    unsafe {
        core::ptr::copy_nonoverlapping(
            core::ptr::with_exposed_provenance(ptr as usize),
            out.as_mut_ptr(),
            out.len(),
        );
    }
    out
}

fn read_linear_prefix<const N: usize>(ptr: u32) -> [u8; N] {
    let mut out = [0u8; N];
    // Safety: callers ensure ptr points to at least N bytes in wasm linear memory.
    unsafe {
        core::ptr::copy_nonoverlapping(
            core::ptr::with_exposed_provenance(ptr as usize),
            out.as_mut_ptr(),
            N,
        );
    }
    out
}

unsafe fn snapshot_scanout_state(ptr: u32) -> ScanoutSnapshot {
    // Implements the same seqlock-style protocol as `aero_shared::scanout_state::ScanoutState::snapshot`,
    // but without depending on the optional `aero-shared` crate when running wasm-bindgen tests.
    let base = core::ptr::with_exposed_provenance::<u32>(ptr as usize);
    loop {
        // Safety: caller ensures `ptr` points to a valid scanout state header in wasm linear memory.
        let gen0 = unsafe { core::ptr::read_volatile(base.add(0)) };
        if (gen0 & SCANOUT_STATE_GENERATION_BUSY_BIT) != 0 {
            core::hint::spin_loop();
            continue;
        }
        let source = unsafe { core::ptr::read_volatile(base.add(1)) };
        let base_lo = unsafe { core::ptr::read_volatile(base.add(2)) };
        let base_hi = unsafe { core::ptr::read_volatile(base.add(3)) };
        let width = unsafe { core::ptr::read_volatile(base.add(4)) };
        let height = unsafe { core::ptr::read_volatile(base.add(5)) };
        let pitch_bytes = unsafe { core::ptr::read_volatile(base.add(6)) };
        let format = unsafe { core::ptr::read_volatile(base.add(7)) };

        let gen1 = unsafe { core::ptr::read_volatile(base.add(0)) };
        if gen0 != gen1 {
            core::hint::spin_loop();
            continue;
        }

        return ScanoutSnapshot {
            generation: gen0,
            source,
            base_paddr: (base_hi as u64) << 32 | base_lo as u64,
            width,
            height,
            pitch_bytes,
            format,
        };
    }
}

#[cfg(feature = "wasm-threaded")]
fn ensure_runtime_reserved_floor() {
    // In shared-memory worker runs the coordinator always instantiates the module with at least the
    // runtime-reserved region available. wasm-bindgen tests may start with a smaller default memory.
    //
    // The scanout state is placed at the end of the runtime-reserved region, so grow memory up to
    // that floor before constructing `Machine` (which takes a reference to the scanout state region
    // in the threaded build).
    const PAGE_BYTES: u32 = 64 * 1024;
    let layout = aero_wasm::guest_ram_layout(0);
    let runtime_reserved = layout.runtime_reserved();
    if runtime_reserved == 0 {
        return;
    }
    let required_pages = runtime_reserved.div_ceil(PAGE_BYTES);
    let current_pages = core::arch::wasm32::memory_size(0) as u32;
    if current_pages < required_pages {
        let delta = required_pages - current_pages;
        let prev = core::arch::wasm32::memory_grow(0, delta as usize);
        assert_ne!(
            prev,
            usize::MAX,
            "wasm memory.grow failed while reserving runtime pages (requested {delta} pages)"
        );
    }
}

fn boot_sector_write_a_to_b8000() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov ax, 0xB800
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xB8]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;
    // mov ax, 0x0020  (' ' with attr 0x00 => black)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x20, 0x00]);
    i += 3;
    // mov cx, 2000  (80*25)
    sector[i..i + 3].copy_from_slice(&[0xB9, 0xD0, 0x07]);
    i += 3;
    // rep stosw
    sector[i..i + 2].copy_from_slice(&[0xF3, 0xAB]);
    i += 2;

    // Disable the hardware text cursor (CRTC cursor start register bit5).
    // mov dx, 0x3D4
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD4, 0x03]);
    i += 3;
    // mov al, 0x0A
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x0A]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // inc dx
    sector[i] = 0x42;
    i += 1;
    // mov al, 0x20 (cursor disable)
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x20]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // Write 'A' with attr 0x1F (white on blue) at the top-left cell.
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;
    // mov ax, 0x1F41
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x41, 0x1F]);
    i += 3;
    // stosw
    sector[i] = 0xAB;
    i += 1;

    // cli; hlt; jmp $
    sector[i] = 0xFA;
    i += 1;
    sector[i] = 0xF4;
    i += 1;
    sector[i..i + 2].copy_from_slice(&[0xEB, 0xFE]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn boot_sector_vbe_64x64x32_red_pixel() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // Program a tiny Bochs VBE mode (64x64x32) and write a red pixel through the 64KiB banked
    // window at 0xA0000 (real-mode accessible).

    // cld (ensure stosb increments DI)
    sector[i] = 0xFC;
    i += 1;

    // mov dx, VBE_DISPI_INDEX_PORT (Bochs VBE index port)
    let [lo, hi] = VBE_DISPI_INDEX_PORT.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xBA, lo, hi]);
    i += 3;

    let write_vbe_reg = |sector: &mut [u8; 512], i: &mut usize, index: u16, value: u16| {
        // dx is expected to be VBE_DISPI_INDEX_PORT here.
        // mov ax, index
        sector[*i..*i + 3].copy_from_slice(&[0xB8, (index & 0xFF) as u8, (index >> 8) as u8]);
        *i += 3;
        // out dx, ax
        sector[*i] = 0xEF;
        *i += 1;
        // inc dx (VBE_DISPI_DATA_PORT)
        sector[*i] = 0x42;
        *i += 1;
        // mov ax, value
        sector[*i..*i + 3].copy_from_slice(&[0xB8, (value & 0xFF) as u8, (value >> 8) as u8]);
        *i += 3;
        // out dx, ax
        sector[*i] = 0xEF;
        *i += 1;
        // dec dx (back to VBE_DISPI_INDEX_PORT)
        sector[*i] = 0x4A;
        *i += 1;
    };

    // XRES = 64
    write_vbe_reg(&mut sector, &mut i, 0x0001, 64);
    // YRES = 64
    write_vbe_reg(&mut sector, &mut i, 0x0002, 64);
    // BPP = 32
    write_vbe_reg(&mut sector, &mut i, 0x0003, 32);
    // ENABLE = 0x0041 (enable + LFB)
    write_vbe_reg(&mut sector, &mut i, 0x0004, 0x0041);
    // BANK = 0
    write_vbe_reg(&mut sector, &mut i, 0x0005, 0);

    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;

    // Write a red pixel at (0,0) in BGRX format expected by the SVGA renderer.
    // mov al, 0x00 ; B
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb ; B
    sector[i] = 0xAA;
    i += 1;
    // stosb ; G (still 0)
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0xFF ; R
    sector[i..i + 2].copy_from_slice(&[0xB0, 0xFF]);
    i += 2;
    // stosb ; R
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0x00 ; X
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb ; X
    sector[i] = 0xAA;
    i += 1;

    // cli; hlt; jmp $
    sector[i] = 0xFA;
    i += 1;
    sector[i] = 0xF4;
    i += 1;
    sector[i..i + 2].copy_from_slice(&[0xEB, 0xFE]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn boot_sector_vbe_64x64x32_strided_and_panned_red_pixel() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // Program a tiny Bochs VBE mode (64x64x32) with a larger virtual width + a display-start
    // offset, then write a red pixel at the top-left of the *visible* window.

    // cld (ensure stosb increments DI)
    sector[i] = 0xFC;
    i += 1;

    // mov dx, VBE_DISPI_INDEX_PORT (Bochs VBE index port)
    let [lo, hi] = VBE_DISPI_INDEX_PORT.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xBA, lo, hi]);
    i += 3;

    let write_vbe_reg = |sector: &mut [u8; 512], i: &mut usize, index: u16, value: u16| {
        // dx is expected to be VBE_DISPI_INDEX_PORT here.
        // mov ax, index
        sector[*i..*i + 3].copy_from_slice(&[0xB8, (index & 0xFF) as u8, (index >> 8) as u8]);
        *i += 3;
        // out dx, ax
        sector[*i] = 0xEF;
        *i += 1;
        // inc dx (VBE_DISPI_DATA_PORT)
        sector[*i] = 0x42;
        *i += 1;
        // mov ax, value
        sector[*i..*i + 3].copy_from_slice(&[0xB8, (value & 0xFF) as u8, (value >> 8) as u8]);
        *i += 3;
        // out dx, ax
        sector[*i] = 0xEF;
        *i += 1;
        // dec dx (back to VBE_DISPI_INDEX_PORT)
        sector[*i] = 0x4A;
        *i += 1;
    };

    // XRES = 64
    write_vbe_reg(&mut sector, &mut i, 0x0001, 64);
    // YRES = 64
    write_vbe_reg(&mut sector, &mut i, 0x0002, 64);
    // BPP = 32
    write_vbe_reg(&mut sector, &mut i, 0x0003, 32);
    // VIRT_WIDTH = 128 (stride in pixels)
    write_vbe_reg(&mut sector, &mut i, 0x0006, 128);
    // X_OFFSET = 8
    write_vbe_reg(&mut sector, &mut i, 0x0008, 8);
    // Y_OFFSET = 2
    write_vbe_reg(&mut sector, &mut i, 0x0009, 2);
    // ENABLE = 0x0041 (enable + LFB)
    write_vbe_reg(&mut sector, &mut i, 0x0004, 0x0041);
    // BANK = 0
    write_vbe_reg(&mut sector, &mut i, 0x0005, 0);

    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // Write a red pixel at (x_offset,y_offset) so it appears at the top-left of the visible
    // window.
    //
    // offset_bytes = (y_offset * virt_width + x_offset) * 4 = (2*128 + 8)*4 = 1056.
    let offset_bytes: u16 = 1056;
    // mov di, offset_bytes
    sector[i..i + 3].copy_from_slice(&[
        0xBF,
        (offset_bytes & 0xFF) as u8,
        (offset_bytes >> 8) as u8,
    ]);
    i += 3;

    // Write a red pixel in BGRX format expected by the SVGA renderer.
    // mov al, 0x00 ; B
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb ; B
    sector[i] = 0xAA;
    i += 1;
    // stosb ; G (still 0)
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0xFF ; R
    sector[i..i + 2].copy_from_slice(&[0xB0, 0xFF]);
    i += 2;
    // stosb ; R
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0x00 ; X
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb ; X
    sector[i] = 0xAA;
    i += 1;

    // cli; hlt; jmp $
    sector[i] = 0xFA;
    i += 1;
    sector[i] = 0xF4;
    i += 1;
    sector[i..i + 2].copy_from_slice(&[0xEB, 0xFE]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn boot_sector_vbe_64x64x8_palette_red_pixel() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // Program a tiny Bochs VBE mode (64x64x8) and write palette index 1 to the first pixel via the
    // banked window at 0xA0000. Then program the VGA DAC palette entry 1 to pure red.

    // cld (ensure stosb increments DI)
    sector[i] = 0xFC;
    i += 1;

    // mov dx, VBE_DISPI_INDEX_PORT (Bochs VBE index port)
    let [lo, hi] = VBE_DISPI_INDEX_PORT.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xBA, lo, hi]);
    i += 3;

    let write_vbe_reg = |sector: &mut [u8; 512], i: &mut usize, index: u16, value: u16| {
        // dx is expected to be VBE_DISPI_INDEX_PORT here.
        // mov ax, index
        sector[*i..*i + 3].copy_from_slice(&[0xB8, (index & 0xFF) as u8, (index >> 8) as u8]);
        *i += 3;
        // out dx, ax
        sector[*i] = 0xEF;
        *i += 1;
        // inc dx (VBE_DISPI_DATA_PORT)
        sector[*i] = 0x42;
        *i += 1;
        // mov ax, value
        sector[*i..*i + 3].copy_from_slice(&[0xB8, (value & 0xFF) as u8, (value >> 8) as u8]);
        *i += 3;
        // out dx, ax
        sector[*i] = 0xEF;
        *i += 1;
        // dec dx (back to VBE_DISPI_INDEX_PORT)
        sector[*i] = 0x4A;
        *i += 1;
    };

    // XRES = 64
    write_vbe_reg(&mut sector, &mut i, 0x0001, 64);
    // YRES = 64
    write_vbe_reg(&mut sector, &mut i, 0x0002, 64);
    // BPP = 8 (palettized)
    write_vbe_reg(&mut sector, &mut i, 0x0003, 8);
    // ENABLE = 0x0041 (enable + LFB)
    write_vbe_reg(&mut sector, &mut i, 0x0004, 0x0041);
    // BANK = 0
    write_vbe_reg(&mut sector, &mut i, 0x0005, 0);

    // Program VGA DAC palette entry 1 to pure red (6-bit DAC values).
    // mov dx, 0x3C6 (PEL mask)
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xC6, 0x03]);
    i += 3;
    // mov al, 0xFF
    sector[i..i + 2].copy_from_slice(&[0xB0, 0xFF]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // mov dx, 0x3C8 (DAC write index)
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xC8, 0x03]);
    i += 3;
    // mov al, 0x01
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x01]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // inc dx (0x3C9 - DAC data port)
    sector[i] = 0x42;
    i += 1;
    // mov al, 63  ; R
    sector[i..i + 2].copy_from_slice(&[0xB0, 63]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // xor al, al  ; G=0, B=0
    sector[i..i + 2].copy_from_slice(&[0x30, 0xC0]);
    i += 2;
    // out dx, al (G)
    sector[i] = 0xEE;
    i += 1;
    // out dx, al (B)
    sector[i] = 0xEE;
    i += 1;

    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;

    // Write palette index 1 at (0,0).
    // mov al, 0x01
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x01]);
    i += 2;
    // stosb
    sector[i] = 0xAA;
    i += 1;

    // cli; hlt; jmp $
    sector[i] = 0xFA;
    i += 1;
    sector[i] = 0xF4;
    i += 1;
    sector[i..i + 2].copy_from_slice(&[0xEB, 0xFE]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn boot_sector_int10_vbe_set_mode_118_hlt() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn boot_sector_vbe_bios_640x480x32_scanline_override_red_pixel() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // Use BIOS VBE calls (INT 10h AX=4Fxx) to:
    // - set a standard VBE mode (0x112 = 640x480x32bpp)
    // - override the logical scanline length in *bytes* via AX=4F06 BL=0x02
    //
    // This exercises the BIOS "scanline override" path. We intentionally request an odd byte
    // length (4101) so the scanline stride is byte-granular (not divisible by 4 bytes-per-pixel)
    // and cannot be represented by the Bochs VBE_DISPI `virt_width` register (pixel-granular).
    //
    // In the threaded WASM build, `aero-wasm` publishes legacy VBE scanout descriptors via
    // `VgaDevice::active_scanout_update()`. That helper only publishes scanout descriptors when
    // the packed-pixel pitch is whole-pixel aligned; unaligned pitches fall back to LegacyText so
    // the host presents the VGA/VBE renderer output instead of attempting scanout readback.

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov ss, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]);
    i += 2;
    // mov sp, 0x7C00
    sector[i..i + 3].copy_from_slice(&[0xBC, 0x00, 0x7C]);
    i += 3;

    // VBE Set Mode (AX=4F02), requesting LFB (bit14) and no-clear (bit15).
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0xC112 (0x112 | 0x4000 | 0x8000)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x12, 0xC1]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // VBE Set/Get Logical Scan Line Length (AX=4F06), subfunction "set in bytes" (BL=0x02).
    // Choose an odd pitch (4101) so the stride is byte-granular (not divisible by 4
    // bytes-per-pixel) and cannot be represented as `virt_width * bytes_per_pixel`
    // (pixel-granular).
    // mov ax, 0x4F06
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x06, 0x4F]);
    i += 3;
    // mov bx, 0x0002  (BL = 0x02)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x02, 0x00]);
    i += 3;
    // mov cx, 0x1005 (4101)
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x05, 0x10]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write a red pixel at the start of the packed-pixel framebuffer via the legacy 0xA0000
    // banked window (real-mode accessible).

    // cld (ensure stosb increments DI)
    sector[i] = 0xFC;
    i += 1;

    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;

    // Write a red pixel in BGRX format expected by the SVGA renderer.
    // mov al, 0x00 ; B
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb ; B
    sector[i] = 0xAA;
    i += 1;
    // stosb ; G (still 0)
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0xFF ; R
    sector[i..i + 2].copy_from_slice(&[0xB0, 0xFF]);
    i += 2;
    // stosb ; R
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0x00 ; X
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb ; X
    sector[i] = 0xAA;
    i += 1;

    // cli; hlt; jmp $
    sector[i] = 0xFA;
    i += 1;
    sector[i] = 0xF4;
    i += 1;
    sector[i..i + 2].copy_from_slice(&[0xEB, 0xFE]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn fnv1a(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn fnv1a_blank_rgba8(len: usize) -> u64 {
    // Blank framebuffer is fully black with alpha=255: [0,0,0,255] repeating.
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for i in 0..len {
        let b = if (i & 3) == 3 { 0xFF } else { 0x00 };
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[wasm_bindgen_test]
fn wasm_machine_vga_present_exposes_nonblank_framebuffer() {
    let boot = boot_sector_write_a_to_b8000();
    // `Machine::new` defaults to the browser canonical configuration (AeroGPU). This test targets
    // the legacy VGA/VBE scanout path, so explicitly enable VGA.
    let mut machine = Machine::new_with_config(16 * 1024 * 1024, false, Some(true), None)
        .expect("Machine::new_with_config");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");
    machine.reset();

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    // Ensure the VGA/SVGA front buffer is up to date before reading it via ptr/len.
    machine.vga_present();

    let width = machine.vga_width();
    let height = machine.vga_height();
    assert!(width > 0, "expected non-zero vga_width");
    assert!(height > 0, "expected non-zero vga_height");
    assert_eq!(machine.vga_stride_bytes(), width * 4);

    let ptr = machine.vga_framebuffer_ptr();
    let len = machine.vga_framebuffer_len_bytes();
    assert!(ptr != 0, "expected non-zero vga_framebuffer_ptr");
    assert!(len != 0, "expected non-zero vga_framebuffer_len_bytes");

    let fb = copy_linear_bytes(ptr, len);
    let hash = fnv1a(&fb);
    let blank = fnv1a_blank_rgba8(len as usize);
    assert_ne!(
        hash, blank,
        "expected VGA framebuffer hash to differ from blank screen"
    );

    // Unified display scanout API should also expose a non-blank framebuffer.
    machine.display_present();
    let disp_w = machine.display_width();
    let disp_h = machine.display_height();
    assert!(disp_w > 0, "expected non-zero display_width");
    assert!(disp_h > 0, "expected non-zero display_height");
    assert_eq!(machine.display_stride_bytes(), disp_w * 4);

    let disp_ptr = machine.display_framebuffer_ptr();
    let disp_len = machine.display_framebuffer_len_bytes();
    assert!(disp_ptr != 0, "expected non-zero display_framebuffer_ptr");
    assert!(
        disp_len != 0,
        "expected non-zero display_framebuffer_len_bytes"
    );

    let disp_fb = copy_linear_bytes(disp_ptr, disp_len);
    let disp_hash = fnv1a(&disp_fb);
    let disp_blank = fnv1a_blank_rgba8(disp_len as usize);
    assert_ne!(
        disp_hash, disp_blank,
        "expected display framebuffer hash to differ from blank screen"
    );
}

#[wasm_bindgen_test]
fn wasm_machine_vbe_present_reports_expected_pixel() {
    #[cfg(feature = "wasm-threaded")]
    ensure_runtime_reserved_floor();

    let boot = boot_sector_vbe_64x64x32_red_pixel();
    // `Machine::new` defaults to AeroGPU; this test specifically targets the legacy VGA/VBE path.
    let mut machine = Machine::new_with_config(16 * 1024 * 1024, false, Some(true), None)
        .expect("Machine::new_with_config");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");

    let scanout_ptr = machine.scanout_state_ptr();
    let scanout_len = machine.scanout_state_len_bytes();

    #[cfg(feature = "wasm-threaded")]
    assert_ne!(
        scanout_ptr, 0,
        "scanout_state_ptr must be non-zero in wasm-threaded builds"
    );

    machine.reset();

    if scanout_ptr == 0 {
        assert_eq!(
            scanout_len, 0,
            "scanout_state_len_bytes should be 0 when scanout_state_ptr is 0"
        );
    } else {
        assert_eq!(
            scanout_len, SCANOUT_STATE_BYTE_LEN,
            "scanout_state_len_bytes should match SCANOUT_STATE_BYTE_LEN"
        );
        let snap = unsafe { snapshot_scanout_state(scanout_ptr) };
        assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
        assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
        assert_ne!(
            snap.generation, 0,
            "Machine::reset should publish legacy text scanout state"
        );
    }

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    if scanout_ptr != 0 {
        let snap = unsafe { snapshot_scanout_state(scanout_ptr) };
        assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
        assert_eq!(snap.base_paddr, u64::from(machine.vbe_lfb_base()));
        assert_eq!(snap.width, 64);
        assert_eq!(snap.height, 64);
        assert_eq!(snap.pitch_bytes, 64 * 4);
        assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
    }

    machine.vga_present();
    assert_eq!(machine.vga_width(), 64);
    assert_eq!(machine.vga_height(), 64);
    assert_eq!(machine.vga_stride_bytes(), 64 * 4);

    let ptr = machine.vga_framebuffer_ptr();
    let len = machine.vga_framebuffer_len_bytes();
    assert!(ptr != 0, "expected non-zero vga_framebuffer_ptr");
    assert_eq!(len, 64 * 64 * 4);

    let pixel = read_linear_prefix::<4>(ptr);
    assert_eq!(&pixel, &[0xFF, 0x00, 0x00, 0xFF]);

    // Unified display API should surface the same scanout.
    machine.display_present();
    assert_eq!(machine.display_width(), 64);
    assert_eq!(machine.display_height(), 64);
    assert_eq!(machine.display_stride_bytes(), 64 * 4);

    let disp_ptr = machine.display_framebuffer_ptr();
    let disp_len = machine.display_framebuffer_len_bytes();
    assert!(disp_ptr != 0, "expected non-zero display_framebuffer_ptr");
    assert_eq!(disp_len, 64 * 64 * 4);
    let disp_pixel = read_linear_prefix::<4>(disp_ptr);
    assert_eq!(&disp_pixel, &[0xFF, 0x00, 0x00, 0xFF]);
}

#[wasm_bindgen_test]
fn wasm_machine_vbe_present_accounts_for_stride_and_panning_in_scanout_state() {
    #[cfg(feature = "wasm-threaded")]
    ensure_runtime_reserved_floor();

    let boot = boot_sector_vbe_64x64x32_strided_and_panned_red_pixel();
    // `Machine::new` defaults to AeroGPU; this test specifically targets the legacy VGA/VBE path.
    let mut machine = Machine::new_with_config(16 * 1024 * 1024, false, Some(true), None)
        .expect("Machine::new_with_config");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");

    let scanout_ptr = machine.scanout_state_ptr();

    #[cfg(feature = "wasm-threaded")]
    assert_ne!(
        scanout_ptr, 0,
        "scanout_state_ptr must be non-zero in wasm-threaded builds"
    );

    machine.reset();

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    if scanout_ptr != 0 {
        let snap = unsafe { snapshot_scanout_state(scanout_ptr) };
        assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
        assert_eq!(snap.width, 64);
        assert_eq!(snap.height, 64);
        assert_eq!(snap.pitch_bytes, 128 * 4);
        assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);

        let lfb_base = u64::from(machine.vbe_lfb_base());
        let expected_base = lfb_base + 2u64 * 128u64 * 4u64 + 8u64 * 4u64;
        assert_eq!(snap.base_paddr, expected_base);
    }

    machine.vga_present();
    assert_eq!(machine.vga_width(), 64);
    assert_eq!(machine.vga_height(), 64);

    let ptr = machine.vga_framebuffer_ptr();
    let len = machine.vga_framebuffer_len_bytes();
    assert!(ptr != 0, "expected non-zero vga_framebuffer_ptr");
    assert_eq!(len, 64 * 64 * 4);

    let pixel = read_linear_prefix::<4>(ptr);
    assert_eq!(&pixel, &[0xFF, 0x00, 0x00, 0xFF]);
}

#[wasm_bindgen_test]
fn wasm_machine_vbe_8bpp_mode_falls_back_to_legacy_text_scanout_state() {
    #[cfg(feature = "wasm-threaded")]
    ensure_runtime_reserved_floor();

    let boot = boot_sector_vbe_64x64x8_palette_red_pixel();
    // `Machine::new` defaults to AeroGPU; this test specifically targets the legacy VGA/VBE path.
    let mut machine = Machine::new_with_config(16 * 1024 * 1024, false, Some(true), None)
        .expect("Machine::new_with_config");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");

    let scanout_ptr = machine.scanout_state_ptr();

    #[cfg(feature = "wasm-threaded")]
    assert_ne!(
        scanout_ptr, 0,
        "scanout_state_ptr must be non-zero in wasm-threaded builds"
    );

    machine.reset();

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    if scanout_ptr != 0 {
        let snap = unsafe { snapshot_scanout_state(scanout_ptr) };
        assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
        assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
        assert_ne!(
            snap.generation, 0,
            "expected reset to publish a legacy scanout update"
        );
    }

    machine.vga_present();
    assert_eq!(machine.vga_width(), 64);
    assert_eq!(machine.vga_height(), 64);
    assert_eq!(machine.vga_stride_bytes(), 64 * 4);

    let ptr = machine.vga_framebuffer_ptr();
    let len = machine.vga_framebuffer_len_bytes();
    assert!(ptr != 0, "expected non-zero vga_framebuffer_ptr");
    assert_eq!(len, 64 * 64 * 4);

    let pixel = read_linear_prefix::<4>(ptr);
    assert_eq!(&pixel, &[0xFF, 0x00, 0x00, 0xFF]);
}

#[wasm_bindgen_test]
fn wasm_machine_vbe_scanline_override_with_unaligned_pitch_falls_back_to_legacy_text_scanout_state()
{
    #[cfg(feature = "wasm-threaded")]
    ensure_runtime_reserved_floor();

    let boot = boot_sector_vbe_bios_640x480x32_scanline_override_red_pixel();
    // `Machine::new` defaults to AeroGPU; this test specifically targets the legacy VGA/VBE path.
    let mut machine = Machine::new_with_config(16 * 1024 * 1024, false, Some(true), None)
        .expect("Machine::new_with_config");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");

    let scanout_ptr = machine.scanout_state_ptr();

    #[cfg(feature = "wasm-threaded")]
    assert_ne!(
        scanout_ptr, 0,
        "scanout_state_ptr must be non-zero in wasm-threaded builds"
    );

    machine.reset();

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    if scanout_ptr != 0 {
        let snap = unsafe { snapshot_scanout_state(scanout_ptr) };
        // The BIOS preserves the requested byte-granular pitch, but the shared scanout descriptor
        // path requires whole-pixel pitch alignment for packed pixels. When the pitch is not
        // pixel-aligned, `aero-wasm` publishes a LegacyText scanout so the host uses the VGA/VBE
        // renderer output instead of attempting scanout readback.
        assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
        assert_eq!(snap.base_paddr, 0);
        assert_eq!(snap.width, 0);
        assert_eq!(snap.height, 0);
        assert_eq!(snap.pitch_bytes, 0);
        assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
    }

    machine.vga_present();
    assert_eq!(machine.vga_width(), 640);
    assert_eq!(machine.vga_height(), 480);

    let ptr = machine.vga_framebuffer_ptr();
    let len = machine.vga_framebuffer_len_bytes();
    assert!(ptr != 0, "expected non-zero vga_framebuffer_ptr");
    assert_eq!(len, 640 * 480 * 4);

    let pixel = read_linear_prefix::<4>(ptr);
    assert_eq!(&pixel, &[0xFF, 0x00, 0x00, 0xFF]);
}

#[wasm_bindgen_test]
fn wasm_machine_aerogpu_config_disables_vga_by_default_and_displays_text_mode() {
    let boot = boot_sector_write_a_to_b8000();
    // Enable AeroGPU via the wasm wrapper and rely on its default to disable VGA.
    let mut machine = Machine::new_with_config(16 * 1024 * 1024, true, None, None)
        .expect("Machine::new_with_config(enable_aerogpu=true)");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");
    machine.reset();

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    // VGA should not be present by default when AeroGPU is enabled.
    assert_eq!(machine.vga_width(), 0);
    assert_eq!(machine.vga_height(), 0);
    assert_eq!(machine.vga_framebuffer_ptr(), 0);
    assert_eq!(machine.vga_framebuffer_len_bytes(), 0);

    // But the unified display scanout should still expose BIOS text mode output by scanning the
    // legacy `0xB8000` text buffer.
    machine.display_present();
    let w = machine.display_width();
    let h = machine.display_height();
    assert!(
        w > 0,
        "expected non-zero display_width in AeroGPU text mode"
    );
    assert!(
        h > 0,
        "expected non-zero display_height in AeroGPU text mode"
    );
    assert_eq!(machine.display_stride_bytes(), w * 4);

    let ptr = machine.display_framebuffer_ptr();
    let len = machine.display_framebuffer_len_bytes();
    assert!(ptr != 0, "expected non-zero display_framebuffer_ptr");
    assert!(len != 0, "expected non-zero display_framebuffer_len_bytes");

    let fb = copy_linear_bytes(ptr, len);
    let hash = fnv1a(&fb);
    let blank = fnv1a_blank_rgba8(len as usize);
    assert_ne!(
        hash, blank,
        "expected AeroGPU text-mode framebuffer hash to differ from blank screen"
    );
}

#[wasm_bindgen_test]
fn wasm_machine_aerogpu_int10_vbe_updates_scanout_state() {
    #[cfg(feature = "wasm-threaded")]
    ensure_runtime_reserved_floor();

    let boot = boot_sector_int10_vbe_set_mode_118_hlt();
    // Enable AeroGPU via the wasm wrapper and rely on its default to disable VGA.
    let mut machine = Machine::new_with_config(16 * 1024 * 1024, true, None, None)
        .expect("Machine::new_with_config(enable_aerogpu=true)");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");

    let scanout_ptr = machine.scanout_state_ptr();
    let scanout_len = machine.scanout_state_len_bytes();
    #[cfg(feature = "wasm-threaded")]
    let cursor_ptr = machine.cursor_state_ptr();
    #[cfg(not(feature = "wasm-threaded"))]
    let cursor_ptr = 0;
    #[cfg(feature = "wasm-threaded")]
    let cursor_len = machine.cursor_state_len_bytes();
    #[cfg(not(feature = "wasm-threaded"))]
    let cursor_len = 0;
    machine.reset();

    // If the build does not support shared scanout state (non-threaded WASM), skip.
    #[cfg(not(feature = "wasm-threaded"))]
    if scanout_ptr == 0 {
        assert_eq!(scanout_len, 0);
        assert_eq!(cursor_ptr, 0);
        assert_eq!(cursor_len, 0);
        return;
    }
    #[cfg(feature = "wasm-threaded")]
    assert_ne!(
        scanout_ptr, 0,
        "scanout_state_ptr must be non-zero in wasm-threaded builds"
    );
    assert_eq!(scanout_len, SCANOUT_STATE_BYTE_LEN);
    #[cfg(feature = "wasm-threaded")]
    assert_ne!(
        cursor_ptr, 0,
        "cursor_state_ptr must be non-zero in wasm-threaded builds"
    );
    #[cfg(feature = "wasm-threaded")]
    assert_eq!(cursor_len, CURSOR_STATE_BYTE_LEN);

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    let snap = unsafe { snapshot_scanout_state(scanout_ptr) };
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    assert_eq!(snap.pitch_bytes, 1024 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
    assert_eq!(snap.base_paddr, u64::from(machine.vbe_lfb_base()));
}
