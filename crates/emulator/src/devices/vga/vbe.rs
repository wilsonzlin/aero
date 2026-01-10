//! VESA VBE 2.0/3.0 data structures and state.
//!
//! This is intentionally focused on the subset of services needed by Windows 7
//! boot graphics (bootmgr/winload/bootvid):
//! - Controller and mode info (INT 10h 4F00/4F01)
//! - Packed-pixel 32bpp modes with a linear framebuffer (LFB)
//! - Banked window A (INT 10h 4F05) for completeness
//! - Scanline/panning and DAC width services (4F06/4F07/4F08)

use core::mem::size_of;

use super::edid;
use memory::{GuestMemory, GuestMemoryResult};

pub fn read_edid(block: u16) -> Option<[u8; edid::EDID_BLOCK_SIZE]> {
    edid::read_edid(block)
}

/// Physical address where the BIOS-visible VBE data blob is written.
///
/// The controller info block contains far pointers to:
/// - the mode list
/// - OEM and product strings
///
/// We place these in conventional memory so real mode code can access them.
pub const VBE_BIOS_DATA_PADDR: u32 = 0x000C_0000;

/// Linear framebuffer base physical address.
///
/// This is intentionally placed in a high, MMIO-like region to avoid
/// conflicting with guest RAM. This matches the typical approach used by
/// virtual GPUs.
pub const VBE_LFB_BASE: u32 = 0xE000_0000;

/// Size of the linear framebuffer aperture.
///
/// Must cover the largest advertised mode with some slack. We target:
/// 1280×1024×4 ≈ 5MiB → allocate 8MiB.
pub const VBE_LFB_SIZE: usize = 8 * 1024 * 1024;

const BIOS_DATA_SEGMENT: u16 = (VBE_BIOS_DATA_PADDR >> 4) as u16;

fn far_ptr(seg: u16, off: u16) -> u32 {
    (seg as u32) << 16 | off as u32
}

/// VBE Controller Information Block (VbeInfoBlock).
///
/// Must be exactly 512 bytes.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct VbeControllerInfo {
    signature: [u8; 4],
    version: u16,
    oem_string_ptr: u32,
    capabilities: [u8; 4],
    video_mode_ptr: u32,
    total_memory: u16,
    oem_software_rev: u16,
    oem_vendor_name_ptr: u32,
    oem_product_name_ptr: u32,
    oem_product_rev_ptr: u32,
    reserved: [u8; 222],
    oem_data: [u8; 256],
}

impl VbeControllerInfo {
    pub fn signature(&self) -> [u8; 4] {
        self.signature
    }

    pub fn version(&self) -> u16 {
        // Packed structs may be unaligned, so use an unaligned read.
        let v = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.version)) };
        u16::from_le(v)
    }

    pub fn video_mode_ptr(&self) -> u32 {
        let v = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.video_mode_ptr)) };
        u32::from_le(v)
    }

    pub fn write_to(&self, dst: &mut [u8]) {
        assert_eq!(dst.len(), size_of::<Self>());
        // SAFETY: `VbeControllerInfo` is `repr(C, packed)` so it has alignment 1.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                dst.as_mut_ptr(),
                size_of::<Self>(),
            );
        }
    }
}

/// VBE Mode Information Block (ModeInfoBlock).
///
/// Must be exactly 256 bytes.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct VbeModeInfo {
    mode_attributes: u16,
    win_a_attributes: u8,
    win_b_attributes: u8,
    win_granularity: u16,
    win_size: u16,
    win_a_segment: u16,
    win_b_segment: u16,
    win_func_ptr: u32,
    bytes_per_scan_line: u16,

    x_resolution: u16,
    y_resolution: u16,
    x_char_size: u8,
    y_char_size: u8,
    number_of_planes: u8,
    bits_per_pixel: u8,
    number_of_banks: u8,
    memory_model: u8,
    bank_size: u8,
    number_of_image_pages: u8,
    reserved1: u8,

    red_mask_size: u8,
    red_field_position: u8,
    green_mask_size: u8,
    green_field_position: u8,
    blue_mask_size: u8,
    blue_field_position: u8,
    reserved_mask_size: u8,
    reserved_field_position: u8,
    direct_color_mode_info: u8,

    phys_base_ptr: u32,
    off_screen_mem_offset: u32,
    off_screen_mem_size: u16,

    reserved2: [u8; 206],
}

impl VbeModeInfo {
    pub fn bits_per_pixel(&self) -> u8 {
        self.bits_per_pixel
    }

    pub fn bytes_per_scan_line(&self) -> u16 {
        let v = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.bytes_per_scan_line)) };
        u16::from_le(v)
    }

    pub fn phys_base_ptr(&self) -> u32 {
        let v = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.phys_base_ptr)) };
        u32::from_le(v)
    }

    pub fn red_mask_size(&self) -> u8 {
        self.red_mask_size
    }
    pub fn red_field_position(&self) -> u8 {
        self.red_field_position
    }
    pub fn green_mask_size(&self) -> u8 {
        self.green_mask_size
    }
    pub fn green_field_position(&self) -> u8 {
        self.green_field_position
    }
    pub fn blue_mask_size(&self) -> u8 {
        self.blue_mask_size
    }
    pub fn blue_field_position(&self) -> u8 {
        self.blue_field_position
    }
    pub fn reserved_mask_size(&self) -> u8 {
        self.reserved_mask_size
    }
    pub fn reserved_field_position(&self) -> u8 {
        self.reserved_field_position
    }

    pub fn write_to(&self, dst: &mut [u8]) {
        assert_eq!(dst.len(), size_of::<Self>());
        // SAFETY: `VbeModeInfo` is `repr(C, packed)` so it has alignment 1.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                dst.as_mut_ptr(),
                size_of::<Self>(),
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VbeModeDescriptor {
    pub mode_id: u16,
    pub width: u16,
    pub height: u16,
    pub bits_per_pixel: u8,
    pub bytes_per_pixel: u8,
    pub pitch_bytes: u16,
    pub memory_model: u8,
}

/// Internal state for VBE services.
#[derive(Debug)]
pub struct VbeState {
    modes: Vec<VbeModeDescriptor>,
    mode_list_ptr: u32,
    oem_string_ptr: u32,
    oem_vendor_name_ptr: u32,
    oem_product_name_ptr: u32,
    oem_product_rev_ptr: u32,

    current_mode: Option<VbeModeDescriptor>,
    lfb_enabled: bool,

    bank_a: u16,
    display_start: (u16, u16),
    logical_scanline_bytes: u16,
    dac_width: u8,

    palette: [u32; 256],
}

impl VbeState {
    pub fn new() -> Self {
        // Deterministic mode list. The 32bpp mode IDs intentionally reuse the
        // common VBE 24bpp numbers. Boot loaders enumerate via 4F00/4F01 and
        // rely on the mode info block (not the numeric ID) to determine bpp.
        let modes = vec![
            // Optional 8bpp paletted modes for compatibility.
            Self::mode(0x101, 640, 480, 8, 4),
            Self::mode(0x103, 800, 600, 8, 4),
            Self::mode(0x105, 1024, 768, 8, 4),
            Self::mode(0x107, 1280, 1024, 8, 4),
            // Packed-pixel 32bpp modes for modern boot graphics.
            Self::mode(0x112, 640, 480, 32, 6),
            Self::mode(0x115, 800, 600, 32, 6),
            Self::mode(0x118, 1024, 768, 32, 6),
            Self::mode(0x11B, 1280, 1024, 32, 6),
        ];

        let palette = core::array::from_fn(|i| {
            let v = i as u8;
            // Default VGA-ish grayscale palette for 8bpp modes (RGBA8888).
            u32::from_le_bytes([v, v, v, 0xFF])
        });

        Self {
            modes,
            mode_list_ptr: 0,
            oem_string_ptr: 0,
            oem_vendor_name_ptr: 0,
            oem_product_name_ptr: 0,
            oem_product_rev_ptr: 0,
            current_mode: None,
            lfb_enabled: false,
            bank_a: 0,
            display_start: (0, 0),
            logical_scanline_bytes: 0,
            dac_width: 8,
            palette,
        }
    }

    const fn mode(
        mode_id: u16,
        width: u16,
        height: u16,
        bpp: u8,
        memory_model: u8,
    ) -> VbeModeDescriptor {
        let bytes_per_pixel = match bpp {
            32 => 4,
            8 => 1,
            _ => 0,
        };
        let pitch_bytes = width as u32 * bytes_per_pixel as u32;
        VbeModeDescriptor {
            mode_id,
            width,
            height,
            bits_per_pixel: bpp,
            bytes_per_pixel,
            pitch_bytes: pitch_bytes as u16,
            memory_model,
        }
    }

    pub fn palette(&self) -> &[u32; 256] {
        &self.palette
    }

    pub fn install_bios_data(&mut self, mem: &mut dyn GuestMemory) -> GuestMemoryResult<()> {
        // Layout inside the BIOS data blob.
        const MODE_LIST_OFF: u16 = 0x0000;
        const OEM_STRING_OFF: u16 = 0x0100;
        const VENDOR_NAME_OFF: u16 = 0x0120;
        const PRODUCT_NAME_OFF: u16 = 0x0140;
        const PRODUCT_REV_OFF: u16 = 0x0160;

        // Mode list (u16 array terminated by 0xFFFF).
        let mut mode_list = Vec::with_capacity((self.modes.len() + 1) * 2);
        for m in &self.modes {
            mode_list.extend_from_slice(&m.mode_id.to_le_bytes());
        }
        mode_list.extend_from_slice(&0xFFFFu16.to_le_bytes());
        mem.write_from(
            VBE_BIOS_DATA_PADDR as u64 + MODE_LIST_OFF as u64,
            &mode_list,
        )?;

        fn write_str(mem: &mut dyn GuestMemory, off: u16, s: &str) -> GuestMemoryResult<()> {
            let mut bytes = Vec::with_capacity(s.len() + 1);
            bytes.extend_from_slice(s.as_bytes());
            bytes.push(0);
            mem.write_from(VBE_BIOS_DATA_PADDR as u64 + off as u64, &bytes)
        }

        write_str(mem, OEM_STRING_OFF, "Aero VBE BIOS")?;
        write_str(mem, VENDOR_NAME_OFF, "Aero")?;
        write_str(mem, PRODUCT_NAME_OFF, "Aero SVGA")?;
        write_str(mem, PRODUCT_REV_OFF, "0.1")?;

        self.mode_list_ptr = far_ptr(BIOS_DATA_SEGMENT, MODE_LIST_OFF);
        self.oem_string_ptr = far_ptr(BIOS_DATA_SEGMENT, OEM_STRING_OFF);
        self.oem_vendor_name_ptr = far_ptr(BIOS_DATA_SEGMENT, VENDOR_NAME_OFF);
        self.oem_product_name_ptr = far_ptr(BIOS_DATA_SEGMENT, PRODUCT_NAME_OFF);
        self.oem_product_rev_ptr = far_ptr(BIOS_DATA_SEGMENT, PRODUCT_REV_OFF);
        Ok(())
    }

    pub fn controller_info(&self) -> VbeControllerInfo {
        // Total video memory in 64KiB blocks.
        let total_mem_blocks = ((VBE_LFB_SIZE + 0xFFFF) / 0x1_0000) as u16;

        VbeControllerInfo {
            signature: *b"VESA",
            version: 0x0300u16.to_le(),
            oem_string_ptr: self.oem_string_ptr.to_le(),
            capabilities: [0; 4],
            video_mode_ptr: self.mode_list_ptr.to_le(),
            total_memory: total_mem_blocks.to_le(),
            oem_software_rev: 0x0100u16.to_le(),
            oem_vendor_name_ptr: self.oem_vendor_name_ptr.to_le(),
            oem_product_name_ptr: self.oem_product_name_ptr.to_le(),
            oem_product_rev_ptr: self.oem_product_rev_ptr.to_le(),
            reserved: [0; 222],
            oem_data: [0; 256],
        }
    }

    pub fn mode_info(&self, mode: u16) -> Option<VbeModeInfo> {
        let mode_id = mode & 0x3FFF;
        let desc = self.modes.iter().find(|m| m.mode_id == mode_id)?;

        // Mode attributes:
        // - supported
        // - color
        // - graphics
        // - banked window available
        // - LFB available for packed pixel modes (requirement calls out bit 7)
        //
        // The exact bit assignments vary across documentation; for this project we
        // follow the requirement explicitly and set 0x0080 for LFB availability.
        let mut mode_attributes: u16 = 0;
        mode_attributes |= 0x0001; // supported
        mode_attributes |= 0x0004; // color
        mode_attributes |= 0x0008; // graphics
        mode_attributes |= 0x0020; // windowed
        if desc.bits_per_pixel >= 8 {
            mode_attributes |= 0x0080; // linear framebuffer available (bit 7)
        }

        let (
            red_mask_size,
            red_field_position,
            green_mask_size,
            green_field_position,
            blue_mask_size,
            blue_field_position,
            reserved_mask_size,
            reserved_field_position,
        ) = if desc.bits_per_pixel == 32 {
            (8, 16, 8, 8, 8, 0, 8, 24)
        } else {
            (0, 0, 0, 0, 0, 0, 0, 0)
        };

        Some(VbeModeInfo {
            mode_attributes: mode_attributes.to_le(),
            win_a_attributes: 0x07, // read/write/relocatable
            win_b_attributes: 0x00,
            win_granularity: 64u16.to_le(),
            win_size: 64u16.to_le(),
            win_a_segment: 0xA000u16.to_le(),
            win_b_segment: 0x0000u16.to_le(),
            win_func_ptr: 0u32.to_le(),
            bytes_per_scan_line: desc.pitch_bytes.to_le(),

            x_resolution: desc.width.to_le(),
            y_resolution: desc.height.to_le(),
            x_char_size: 8,
            y_char_size: 16,
            number_of_planes: 1,
            bits_per_pixel: desc.bits_per_pixel,
            number_of_banks: 1,
            memory_model: desc.memory_model,
            bank_size: 0,
            number_of_image_pages: 0,
            reserved1: 0,

            red_mask_size,
            red_field_position,
            green_mask_size,
            green_field_position,
            blue_mask_size,
            blue_field_position,
            reserved_mask_size,
            reserved_field_position,
            direct_color_mode_info: 0,

            phys_base_ptr: VBE_LFB_BASE.to_le(),
            off_screen_mem_offset: 0u32.to_le(),
            off_screen_mem_size: 0u16.to_le(),

            reserved2: [0; 206],
        })
    }

    pub fn set_mode(&mut self, mode: u16) -> Result<(), &'static str> {
        let mode_id = mode & 0x3FFF;
        let lfb = (mode & 0x4000) != 0;

        let desc = self
            .modes
            .iter()
            .find(|m| m.mode_id == mode_id)
            .copied()
            .ok_or("unsupported VBE mode")?;

        self.current_mode = Some(desc);
        self.lfb_enabled = lfb;
        self.bank_a = 0;
        self.display_start = (0, 0);
        self.logical_scanline_bytes = desc.pitch_bytes;
        Ok(())
    }

    pub fn is_lfb_enabled(&self) -> bool {
        self.lfb_enabled
    }

    pub fn current_mode(&self) -> Option<VbeModeDescriptor> {
        self.current_mode
    }

    pub fn resolution(&self) -> Option<(u16, u16)> {
        self.current_mode.map(|m| (m.width, m.height))
    }

    pub fn pitch_bytes(&self) -> Option<u16> {
        self.current_mode.map(|m| m.pitch_bytes)
    }

    pub fn bank_a(&self) -> u16 {
        self.bank_a
    }

    pub fn set_bank_a(&mut self, bank: u16) {
        self.bank_a = bank;
    }

    pub fn logical_scanline_bytes(&self) -> u16 {
        self.logical_scanline_bytes
    }

    pub fn set_logical_scanline_bytes(&mut self, bytes: u16) -> u16 {
        // Keep behavior deterministic and within the LFB aperture.
        // Real hardware may clamp based on maximum virtual resolution.
        let pitch = self.current_mode.map(|m| m.pitch_bytes).unwrap_or(0);
        let bytes = bytes.max(pitch);
        self.logical_scanline_bytes = bytes;
        bytes
    }

    pub fn display_start(&self) -> (u16, u16) {
        self.display_start
    }

    pub fn set_display_start(&mut self, x: u16, y: u16) {
        self.display_start = (x, y);
    }

    pub fn dac_width(&self) -> u8 {
        self.dac_width
    }

    pub fn set_dac_width(&mut self, width: u8) -> u8 {
        let width = width.clamp(6, 8);
        self.dac_width = width;
        width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_spec() {
        assert_eq!(size_of::<VbeControllerInfo>(), 512);
        assert_eq!(size_of::<VbeModeInfo>(), 256);
    }
}
