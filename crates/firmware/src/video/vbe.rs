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
    // With the default 16MiB BIOS memory size this leaves enough headroom for 1024×768×32bpp.
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
        ];

        Self {
            current_mode: None,
            lfb_base: Self::LFB_BASE_DEFAULT,
            bank: 0,
            logical_width_pixels: 0,
            bytes_per_scan_line: 0,
            display_start_x: 0,
            display_start_y: 0,
            dac_width_bits: 6,
            palette: [0; 256 * 4],
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

        // Total memory in 64KB blocks. Provide 16MB.
        buf[18..20].copy_from_slice(&256u16.to_le_bytes());

        // VBE 2.0+ fields.
        buf[20..22].copy_from_slice(&0x0001u16.to_le_bytes()); // OEM software rev
        buf[22..26].copy_from_slice(&vendor_ptr.to_le_bytes());
        buf[26..30].copy_from_slice(&product_ptr.to_le_bytes());
        buf[30..34].copy_from_slice(&product_rev_ptr.to_le_bytes());

        mem.write_bytes(dest, &buf);
    }

    pub fn write_mode_info(&self, mem: &mut impl MemoryBus, mode: u16, dest: u64) -> bool {
        let mode = match self.find_mode(mode) {
            Some(mode) => mode,
            None => return false,
        };

        let mut buf = [0u8; 256];

        // ModeAttributes: supported | info | color | graphics | LFB
        let mode_attributes: u16 = 0x009B;
        buf[0..2].copy_from_slice(&mode_attributes.to_le_bytes());

        // Windowing (banked framebuffer). Provide a 64KB window at A000:0000.
        buf[2] = 0x07; // WinAAttributes: readable/writeable
        buf[3] = 0x00; // WinBAttributes: not supported
        buf[4..6].copy_from_slice(&64u16.to_le_bytes()); // WinGranularity in KB
        buf[6..8].copy_from_slice(&64u16.to_le_bytes()); // WinSize in KB
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
        buf[26] = 1; // NumberOfBanks
        buf[27] = mode.memory_model;
        buf[28] = 0; // BankSize (unused with 64KB granularity)
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
        // Report a conservative maximum based on 16MB of video memory.
        let bytes_per_line = self.bytes_per_scan_line.max(1) as u32;
        let max_lines = (16u32 * 1024 * 1024) / bytes_per_line;
        max_lines.min(u16::MAX as u32) as u16
    }
}
