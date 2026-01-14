use crate::memory::{make_far_ptr, real_addr, MemoryBus};

#[derive(Debug, Clone, Copy)]
pub struct VbeMode {
    pub mode: u16,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_field_position: u8,
    pub green_mask_size: u8,
    pub green_field_position: u8,
    pub blue_mask_size: u8,
    pub blue_field_position: u8,
    pub rsvd_mask_size: u8,
    pub rsvd_field_position: u8,
}

impl VbeMode {
    pub const fn bytes_per_pixel(self) -> u16 {
        (self.bpp as u16).div_ceil(8)
    }

    pub const fn bytes_per_scan_line(self) -> u16 {
        self.width * self.bytes_per_pixel()
    }

    pub const fn framebuffer_size_bytes(self) -> u32 {
        self.bytes_per_scan_line() as u32 * self.height as u32
    }
}

#[derive(Debug, Clone)]
pub struct VbeDevice {
    pub current_mode: Option<u16>,
    pub lfb_base: u32,
    /// Total video memory reported via VBE "Get Controller Info" (AX=4F00h), in 64KiB blocks.
    ///
    /// The VBE spec stores this as a 16-bit count of 64KiB blocks (`TotalMemory`), so the maximum
    /// representable value is just under 4GiB.
    ///
    /// By default, Aero reports 16MiB to match the canonical `aero_gpu_vga` boot display device
    /// model. Machine integrations that place the linear framebuffer inside a larger VRAM
    /// aperture (e.g. AeroGPU BAR1) should override this so guests that sanity-check the VBE
    /// memory size observe a consistent value.
    pub total_memory_64kb_blocks: u16,
    pub bank: u16,
    pub logical_width_pixels: u16,
    pub bytes_per_scan_line: u16,
    pub display_start_x: u16,
    pub display_start_y: u16,
    pub dac_width_bits: u8,
    pub palette: [u8; 256 * 4],
    modes: &'static [VbeMode],
}

impl Default for VbeDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl VbeDevice {
    pub const VBE_INFO_SEGMENT: u16 = 0xC000;
    pub const OEM_STRING_OFFSET: u16 = 0x0000;
    pub const VENDOR_STRING_OFFSET: u16 = 0x0020;
    pub const PRODUCT_STRING_OFFSET: u16 = 0x0040;
    pub const PRODUCT_REV_STRING_OFFSET: u16 = 0x0060;
    pub const MODE_LIST_OFFSET: u16 = 0x0080;

    // Keep the linear framebuffer inside conventional guest RAM so the machine-based BIOS tests
    // (which use a plain `PhysicalMemory` backing) can access it without MMIO routing.
    //
    // With the default 16MiB BIOS memory size this leaves enough headroom for the largest
    // advertised 32bpp mode (currently 1280×1024×32bpp).
    pub const LFB_BASE_DEFAULT: u32 = 0x0080_0000;

    pub fn new() -> Self {
        const MODES: &[VbeMode] = &[
            VbeMode {
                mode: 0x101,
                width: 640,
                height: 480,
                bpp: 8,
                memory_model: 0x04, // packed pixel
                red_mask_size: 0,
                red_field_position: 0,
                green_mask_size: 0,
                green_field_position: 0,
                blue_mask_size: 0,
                blue_field_position: 0,
                rsvd_mask_size: 0,
                rsvd_field_position: 0,
            },
            VbeMode {
                mode: 0x103,
                width: 800,
                height: 600,
                bpp: 8,
                memory_model: 0x04,
                red_mask_size: 0,
                red_field_position: 0,
                green_mask_size: 0,
                green_field_position: 0,
                blue_mask_size: 0,
                blue_field_position: 0,
                rsvd_mask_size: 0,
                rsvd_field_position: 0,
            },
            VbeMode {
                mode: 0x105,
                width: 1024,
                height: 768,
                bpp: 8,
                memory_model: 0x04,
                red_mask_size: 0,
                red_field_position: 0,
                green_mask_size: 0,
                green_field_position: 0,
                blue_mask_size: 0,
                blue_field_position: 0,
                rsvd_mask_size: 0,
                rsvd_field_position: 0,
            },
            VbeMode {
                mode: 0x112,
                width: 640,
                height: 480,
                bpp: 32,
                memory_model: 0x06, // direct color
                red_mask_size: 8,
                red_field_position: 16,
                green_mask_size: 8,
                green_field_position: 8,
                blue_mask_size: 8,
                blue_field_position: 0,
                rsvd_mask_size: 8,
                rsvd_field_position: 24,
            },
            VbeMode {
                mode: 0x115,
                width: 800,
                height: 600,
                bpp: 32,
                memory_model: 0x06, // direct color
                red_mask_size: 8,
                red_field_position: 16,
                green_mask_size: 8,
                green_field_position: 8,
                blue_mask_size: 8,
                blue_field_position: 0,
                rsvd_mask_size: 8,
                rsvd_field_position: 24,
            },
            VbeMode {
                mode: 0x118,
                width: 1024,
                height: 768,
                bpp: 32,
                memory_model: 0x06, // direct color
                red_mask_size: 8,
                red_field_position: 16,
                green_mask_size: 8,
                green_field_position: 8,
                blue_mask_size: 8,
                blue_field_position: 0,
                rsvd_mask_size: 8,
                rsvd_field_position: 24,
            },
            VbeMode {
                mode: 0x11B,
                width: 1280,
                height: 1024,
                bpp: 32,
                memory_model: 0x06, // direct color
                red_mask_size: 8,
                red_field_position: 16,
                green_mask_size: 8,
                green_field_position: 8,
                blue_mask_size: 8,
                blue_field_position: 0,
                rsvd_mask_size: 8,
                rsvd_field_position: 24,
            },
            VbeMode {
                // OEM-defined mode ID used by the docs for "1280×720×32bpp".
                mode: 0x160,
                width: 1280,
                height: 720,
                bpp: 32,
                memory_model: 0x06, // direct color
                red_mask_size: 8,
                red_field_position: 16,
                green_mask_size: 8,
                green_field_position: 8,
                blue_mask_size: 8,
                blue_field_position: 0,
                rsvd_mask_size: 8,
                rsvd_field_position: 24,
            },
        ];

        Self {
            current_mode: None,
            lfb_base: Self::LFB_BASE_DEFAULT,
            // 16MiB / 64KiB = 256 blocks.
            total_memory_64kb_blocks: 256,
            bank: 0,
            logical_width_pixels: 0,
            bytes_per_scan_line: 0,
            display_start_x: 0,
            display_start_y: 0,
            dac_width_bits: 6,
            // Initialize the VBE palette to a VGA-like default so INT 10h AX=4F09 "Get Palette
            // Data" returns meaningful values (even before the guest programs the palette).
            //
            // Entry layout is B, G, R, 0 with 6-bit components by default.
            palette: default_vga_palette_bgr0_6bit(),
            modes: MODES,
        }
    }

    pub fn supported_modes(&self) -> impl Iterator<Item = VbeMode> + '_ {
        self.modes.iter().copied()
    }

    pub fn find_mode(&self, mode: u16) -> Option<VbeMode> {
        self.modes.iter().copied().find(|m| m.mode == mode)
    }

    pub fn write_oem_data(&self, mem: &mut impl MemoryBus) {
        let base = real_addr(Self::VBE_INFO_SEGMENT, 0);

        mem.write_bytes(base + Self::OEM_STRING_OFFSET as u64, b"Aero VBE BIOS\0");
        mem.write_bytes(base + Self::VENDOR_STRING_OFFSET as u64, b"Aero\0");
        mem.write_bytes(base + Self::PRODUCT_STRING_OFFSET as u64, b"Aero SVGA\0");
        mem.write_bytes(base + Self::PRODUCT_REV_STRING_OFFSET as u64, b"0.1\0");

        let mut off = base + Self::MODE_LIST_OFFSET as u64;
        for mode in self.modes.iter().map(|m| m.mode) {
            mem.write_u16(off, mode);
            off += 2;
        }
        mem.write_u16(off, 0xFFFF);
    }

    pub fn write_controller_info(&self, mem: &mut impl MemoryBus, dest: u64) {
        self.write_oem_data(mem);

        let oem_ptr = make_far_ptr(Self::VBE_INFO_SEGMENT, Self::OEM_STRING_OFFSET);
        let vendor_ptr = make_far_ptr(Self::VBE_INFO_SEGMENT, Self::VENDOR_STRING_OFFSET);
        let product_ptr = make_far_ptr(Self::VBE_INFO_SEGMENT, Self::PRODUCT_STRING_OFFSET);
        let product_rev_ptr = make_far_ptr(Self::VBE_INFO_SEGMENT, Self::PRODUCT_REV_STRING_OFFSET);
        let mode_ptr = make_far_ptr(Self::VBE_INFO_SEGMENT, Self::MODE_LIST_OFFSET);

        let mut buf = [0u8; 512];
        buf[0..4].copy_from_slice(b"VESA");
        buf[4..6].copy_from_slice(&0x0300u16.to_le_bytes()); // VBE 3.0 (superset of 2.0)
        buf[6..10].copy_from_slice(&oem_ptr.to_le_bytes());

        // Capabilities: bit0 = DAC width switchable.
        buf[10..14].copy_from_slice(&1u32.to_le_bytes());
        buf[14..18].copy_from_slice(&mode_ptr.to_le_bytes());

        // Total memory in 64KB blocks.
        buf[18..20].copy_from_slice(&self.total_memory_64kb_blocks.to_le_bytes());

        // VBE 2.0+ fields.
        buf[20..22].copy_from_slice(&0x0001u16.to_le_bytes()); // OEM software rev
        buf[22..26].copy_from_slice(&vendor_ptr.to_le_bytes());
        buf[26..30].copy_from_slice(&product_ptr.to_le_bytes());
        buf[30..34].copy_from_slice(&product_rev_ptr.to_le_bytes());

        mem.write_bytes(dest, &buf);
    }

    pub fn write_mode_info(&self, mem: &mut impl MemoryBus, mode: u16, dest: u64) -> bool {
        // VBE function 4F01 passes a 14-bit mode number; callers sometimes preserve the "mode set"
        // flag bits (e.g. bit14 = LFB requested) when querying mode info. Mask off the high bits so
        // we accept both `0x0118` and `0x4118`.
        let mode_id = mode & 0x3FFF;
        let mode = match self.find_mode(mode_id) {
            Some(mode) => mode,
            None => return false,
        };

        let mut buf = [0u8; 256];

        // VBE ModeInfoBlock::ModeAttributes (VBE 2.0+).
        //
        // The Windows boot stack (bootmgr/winload/bootvid) is sensitive to these flags; in
        // particular it expects the LFB-available bit when `PhysBasePtr` is non-zero.
        //
        // Bit meanings used here follow the project-wide convention in the emulator VBE
        // implementation (`crates/emulator/src/devices/vga/vbe.rs`):
        // - bit 0: mode supported
        // - bit 2: color mode
        // - bit 3: graphics mode
        // - bit 5: windowed/banked framebuffer available (WinA/WinB fields valid)
        // - bit 7: linear framebuffer available (PhysBasePtr valid)
        const MODE_ATTR_SUPPORTED: u16 = 1 << 0;
        const MODE_ATTR_COLOR: u16 = 1 << 2;
        const MODE_ATTR_GRAPHICS: u16 = 1 << 3;
        const MODE_ATTR_WINDOWED: u16 = 1 << 5;
        const MODE_ATTR_LFB: u16 = 1 << 7;

        let mut mode_attributes: u16 = MODE_ATTR_SUPPORTED | MODE_ATTR_COLOR | MODE_ATTR_GRAPHICS;

        // Banked window A is advertised below (A000:0000), so set the "windowed available" bit.
        mode_attributes |= MODE_ATTR_WINDOWED;

        // All advertised modes expose a linear framebuffer at `PhysBasePtr`.
        mode_attributes |= MODE_ATTR_LFB;
        buf[0..2].copy_from_slice(&mode_attributes.to_le_bytes());

        // Windowing (banked framebuffer).
        //
        // Provide a 64KiB banked window at A000:0000. This is used by real-mode code (and some
        // bootloaders) that cannot directly address the high linear framebuffer.
        const BANK_WINDOW_SIZE_KB: u16 = 64;
        const BANK_WINDOW_SIZE_BYTES: u32 = BANK_WINDOW_SIZE_KB as u32 * 1024;
        buf[2] = 0x07; // WinAAttributes: readable/writeable
        buf[3] = 0x00; // WinBAttributes: not supported
        buf[4..6].copy_from_slice(&BANK_WINDOW_SIZE_KB.to_le_bytes()); // WinGranularity in KB
        buf[6..8].copy_from_slice(&BANK_WINDOW_SIZE_KB.to_le_bytes()); // WinSize in KB
        buf[8..10].copy_from_slice(&0xA000u16.to_le_bytes()); // WinASegment
        buf[10..12].copy_from_slice(&0u16.to_le_bytes()); // WinBSegment
        buf[12..16].copy_from_slice(&0u32.to_le_bytes()); // WinFuncPtr

        buf[16..18].copy_from_slice(&mode.bytes_per_scan_line().to_le_bytes());
        buf[18..20].copy_from_slice(&mode.width.to_le_bytes());
        buf[20..22].copy_from_slice(&mode.height.to_le_bytes());
        buf[22] = 8; // XCharSize
        buf[23] = 16; // YCharSize
        buf[24] = 1; // NumberOfPlanes
        buf[25] = mode.bpp; // BitsPerPixel
                            // NumberOfBanks / BankSize are only meaningful for banked access. We still populate them
                            // consistently so software that falls back to windowed access can determine the address
                            // range.
        let bank_count = mode
            .framebuffer_size_bytes()
            .div_ceil(BANK_WINDOW_SIZE_BYTES)
            .min(u8::MAX as u32) as u8;
        buf[26] = bank_count.max(1);
        buf[27] = mode.memory_model;
        buf[28] = BANK_WINDOW_SIZE_KB as u8; // BankSize in KB
        buf[29] = 0; // NumberOfImagePages
        buf[30] = 0; // Reserved1

        buf[31] = mode.red_mask_size;
        buf[32] = mode.red_field_position;
        buf[33] = mode.green_mask_size;
        buf[34] = mode.green_field_position;
        buf[35] = mode.blue_mask_size;
        buf[36] = mode.blue_field_position;
        buf[37] = mode.rsvd_mask_size;
        buf[38] = mode.rsvd_field_position;
        buf[39] = 0; // DirectColorModeInfo

        buf[40..44].copy_from_slice(&self.lfb_base.to_le_bytes()); // PhysBasePtr
        buf[44..48].copy_from_slice(&0u32.to_le_bytes()); // OffScreenMemOffset
        buf[48..50].copy_from_slice(&0u16.to_le_bytes()); // OffScreenMemSize

        mem.write_bytes(dest, &buf);
        true
    }

    pub fn set_mode(&mut self, mem: &mut impl MemoryBus, mode: u16, no_clear: bool) -> bool {
        let mode = match self.find_mode(mode) {
            Some(mode) => mode,
            None => return false,
        };

        self.current_mode = Some(mode.mode);
        self.bank = 0;
        self.display_start_x = 0;
        self.display_start_y = 0;
        self.logical_width_pixels = mode.width;
        self.bytes_per_scan_line = mode.bytes_per_scan_line();

        if !no_clear {
            // Clear the framebuffer efficiently using bulk writes when available.
            //
            // Note: The firmware memory bus interface supports byte writes as the lowest common
            // denominator, but most implementations also provide an efficient `write_physical`
            // fast path (RAM memcpy / aligned MMIO chunking). Use a fixed-size zero buffer and
            // write in chunks to avoid issuing millions of individual `write_u8` operations.
            const CLEAR_CHUNK: [u8; 4096] = [0u8; 4096];

            let size = mode.framebuffer_size_bytes() as usize;
            let base = self.lfb_base as u64;
            let mut offset = 0usize;
            while offset < size {
                let len = (size - offset).min(CLEAR_CHUNK.len());
                let Some(addr) = base.checked_add(offset as u64) else {
                    break;
                };
                mem.write_physical(addr, &CLEAR_CHUNK[..len]);
                offset += len;
            }
        }

        true
    }

    pub fn max_scan_lines(&self) -> u16 {
        // Report a conservative maximum based on the advertised VBE total memory.
        let bytes_per_line = self.bytes_per_scan_line.max(1) as u32;
        let total_bytes = u32::from(self.total_memory_64kb_blocks) * 64 * 1024;
        let max_lines = total_bytes / bytes_per_line;
        max_lines.min(u16::MAX as u32) as u16
    }
}

fn default_vga_palette_bgr0_6bit() -> [u8; 256 * 4] {
    // VGA BIOSes typically initialize the 256-color DAC with:
    // - EGA 16-color palette in indices 0..=15
    // - a 6×6×6 color cube in indices 16..=231
    // - a grayscale ramp in indices 232..=255
    //
    // We store palette entries as B, G, R, 0 with 6-bit components (0..=63), matching the BIOS
    // default `dac_width_bits=6`.
    let mut pal = [0u8; 256 * 4];

    let mut set = |idx: usize, r8: u8, g8: u8, b8: u8| {
        let base = idx * 4;
        pal[base] = b8 >> 2;
        pal[base + 1] = g8 >> 2;
        pal[base + 2] = r8 >> 2;
        // base+3 is reserved/unused (kept at 0).
    };

    // Standard EGA 16 colors (in 8-bit form, downscaled to 6-bit).
    let ega: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), // 0 black
        (0x00, 0x00, 0xAA), // 1 blue
        (0x00, 0xAA, 0x00), // 2 green
        (0x00, 0xAA, 0xAA), // 3 cyan
        (0xAA, 0x00, 0x00), // 4 red
        (0xAA, 0x00, 0xAA), // 5 magenta
        (0xAA, 0x55, 0x00), // 6 brown
        (0xAA, 0xAA, 0xAA), // 7 light grey
        (0x55, 0x55, 0x55), // 8 dark grey
        (0x55, 0x55, 0xFF), // 9 bright blue
        (0x55, 0xFF, 0x55), // 10 bright green
        (0x55, 0xFF, 0xFF), // 11 bright cyan
        (0xFF, 0x55, 0x55), // 12 bright red
        (0xFF, 0x55, 0xFF), // 13 bright magenta
        (0xFF, 0xFF, 0x55), // 14 yellow
        (0xFF, 0xFF, 0xFF), // 15 white
    ];
    for (i, (r, g, b)) in ega.into_iter().enumerate() {
        set(i, r, g, b);
    }

    // 6×6×6 color cube (indices 16..231), similar to the classic VGA palette.
    let mut idx = 16usize;
    for r in 0..6u8 {
        for g in 0..6u8 {
            for b in 0..6u8 {
                let scale = |v: u8| -> u8 { ((v as u16 * 255) / 5) as u8 };
                set(idx, scale(r), scale(g), scale(b));
                idx += 1;
            }
        }
    }

    // Grayscale ramp (232..255).
    for i in 0..24u8 {
        let v = ((i as u16 * 255) / 23) as u8;
        set(232 + i as usize, v, v, v);
    }

    pal
}
