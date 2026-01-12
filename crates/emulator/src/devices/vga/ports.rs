use core::cell::Cell;

use crate::io::PortIO;

use super::regs::{
    VgaDerivedState, VgaPlanarShift, AC_REGS_INITIAL_LEN, CRTC_REGS_INITIAL_LEN,
    GC_REGS_INITIAL_LEN, POWER_ON_MISC_OUTPUT, SEQ_REGS_INITIAL_LEN,
};
use super::timing::VgaTiming;
use super::vbe::{VbeControllerInfo, VbeModeInfo, VbeState, VBE_LFB_SIZE};
use super::VgaMemory;
use super::{modeset, LegacyBdaInfo};

// Legacy VGA I/O ports span both the mono (0x3Bx) and colour (0x3Dx) decode ranges.
// This device implements the common registers across the full 0x3B0..=0x3DF window.
const PORT_MISC_OUTPUT_WRITE: u16 = 0x3C2;
const PORT_MISC_OUTPUT_READ: u16 = 0x3CC;
const PORT_VIDEO_SUBSYSTEM_ENABLE: u16 = 0x3C3;
const PORT_FEATURE_CONTROL_READ: u16 = 0x3CA;

const PORT_SEQ_INDEX: u16 = 0x3C4;
const PORT_SEQ_DATA: u16 = 0x3C5;

const PORT_GC_INDEX: u16 = 0x3CE;
const PORT_GC_DATA: u16 = 0x3CF;

const PORT_AC_INDEX_DATA: u16 = 0x3C0;
const PORT_AC_DATA_READ: u16 = 0x3C1;

const PORT_INPUT_STATUS1_COLOR: u16 = 0x3DA;
const PORT_INPUT_STATUS1_MONO: u16 = 0x3BA;

const PORT_CRTC_INDEX_COLOR: u16 = 0x3D4;
const PORT_CRTC_DATA_COLOR: u16 = 0x3D5;
const PORT_CRTC_INDEX_MONO: u16 = 0x3B4;
const PORT_CRTC_DATA_MONO: u16 = 0x3B5;

/// Value returned from unimplemented reads in the legacy VGA register range (0x3B0..=0x3DF).
///
/// Chosen to emulate a floating/pulled-up ISA bus (common behaviour for
/// unmapped ports).
const UNIMPLEMENTED_READ_VALUE: u8 = 0xFF;

#[derive(Debug)]
pub struct VgaDevice {
    misc_output: u8,
    /// Video Subsystem Enable register (port `0x3C3`).
    ///
    /// When bit 0 is cleared, writes to other VGA ports are ignored.
    video_subsystem_enable: u8,
    feature_control: u8,

    seq_index: u8,
    pub(crate) seq_regs: Vec<u8>,

    gc_index: u8,
    pub(crate) gc_regs: Vec<u8>,

    crtc_index: u8,
    pub(crate) crtc_regs: Vec<u8>,

    ac_index: u8,
    pub(crate) ac_regs: Vec<u8>,
    /// Attribute controller address/data flip-flop (false = expecting index).
    ac_flip_flop_data: Cell<bool>,
    ac_display_enabled: bool,

    timing: VgaTiming,

    /// 256 KiB VGA RAM, stored as four 64 KiB planes.
    vram: Vec<u8>,

    legacy_bda: LegacyBdaInfo,

    derived: VgaDerivedState,

    // --- VBE / SVGA state ---
    vbe: VbeState,
    lfb: Vec<u8>,
    frontbuffer: Vec<u32>,
    fb_dirty: bool,
}

impl Default for VgaDevice {
    fn default() -> Self {
        Self::new_with_bios_mode3_defaults()
    }
}

impl VgaDevice {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a VGA device in a minimal "power-on reset" state.
    ///
    /// This is intentionally sparse; the BIOS is expected to program a real
    /// mode (commonly mode 3) during POST.
    pub fn new_power_on_reset() -> Self {
        let mut vga = Self::new_with_bios_mode3_defaults();
        vga.reset_power_on();
        vga
    }

    /// Read a byte from the active VGA aperture, returning `None` if the address does not hit VGA
    /// VRAM (as selected by GC reg 0x06).
    pub fn mem_read_u8(&self, vram: &mut VgaMemory, paddr: u64) -> Option<u8> {
        vram.read_u8(paddr, &self.seq_regs, &self.gc_regs)
    }

    /// Write a byte to the active VGA aperture, returning whether the address hit VGA VRAM (as
    /// selected by GC reg 0x06).
    pub fn mem_write_u8(&self, vram: &mut VgaMemory, paddr: u64, value: u8) -> bool {
        vram.write_u8(paddr, value, &self.seq_regs, &self.gc_regs)
    }

    pub fn derived_state(&self) -> VgaDerivedState {
        self.derived
    }

    pub fn vram(&self) -> &[u8] {
        &self.vram
    }

    pub fn legacy_bda_info(&self) -> LegacyBdaInfo {
        self.legacy_bda
    }

    /// Program VGA registers for a legacy BIOS mode (03h/12h/13h...).
    ///
    /// This updates the same internal register storage used by port I/O, so
    /// later reads via the VGA I/O ports reflect the programmed values.
    pub fn set_legacy_mode(&mut self, mode: u8, clear: bool) {
        let table = modeset::legacy_mode_table(mode)
            .unwrap_or_else(|| panic!("unsupported VGA legacy mode 0x{mode:02X}"));

        self.misc_output = table.misc_output;

        self.seq_regs.resize(SEQ_REGS_INITIAL_LEN, 0);
        self.seq_regs.copy_from_slice(&table.sequencer);

        self.gc_regs.resize(GC_REGS_INITIAL_LEN, 0);
        self.gc_regs.copy_from_slice(&table.graphics_controller);

        self.crtc_regs.resize(CRTC_REGS_INITIAL_LEN, 0);
        self.crtc_regs.copy_from_slice(&table.crtc);

        if self.crtc_regs.len() > 0x11 {
            // Real VGA hardware protects CRTC registers 0..=7 when CRTC[0x11].7 is set.
            // VGA BIOSes typically clear it while programming the mode tables and then
            // restore it afterwards. We keep the final value from the mode table, but
            // still force CRTC[0x03].7 which is set in all standard VGA modes.
            let protect = self.crtc_regs[0x11] & 0x80;
            self.crtc_regs[0x11] &= !0x80; // unlock (no-op for direct register writes)
            self.crtc_regs[0x11] |= protect; // restore protect bit from table
            self.crtc_regs[0x03] |= 0x80;
        }

        self.ac_regs.resize(AC_REGS_INITIAL_LEN, 0);
        self.ac_regs.copy_from_slice(&table.attribute_controller);
        self.ac_display_enabled = true;
        self.ac_index = 0;
        self.ac_flip_flop_data.set(false);

        self.seq_index = 0;
        self.gc_index = 0;
        self.crtc_index = 0;

        self.legacy_bda = table.bda_info;

        self.recompute_derived_state();

        if clear {
            self.clear_vram_for_mode();
        }
    }

    fn clear_vram_for_mode(&mut self) {
        if !self.derived.is_graphics {
            // Text mode: odd/even addressing, plane 0 = character, plane 1 = attribute.
            self.vram[0..0x4000].fill(0x20);
            self.vram[0x10000..0x10000 + 0x4000].fill(0x07);
            return;
        }

        if self.derived.chain4 {
            // Chain-4: 64 KiB window maps to 16 KiB in each plane.
            for plane in 0..4 {
                let base = plane * 0x10000;
                self.vram[base..base + 0x4000].fill(0);
            }
            return;
        }

        // Planar modes: clear all planes.
        self.vram.fill(0);
    }

    fn new_with_bios_mode3_defaults() -> Self {
        let mut vga = Self {
            // VGA Misc Output Register bit 0 selects the I/O base:
            // - 0 = 0x3Bx (mono)
            // - 1 = 0x3Dx (colour)
            misc_output: POWER_ON_MISC_OUTPUT,
            video_subsystem_enable: 0x01,
            feature_control: 0x00,

            seq_index: 0,
            seq_regs: vec![0; SEQ_REGS_INITIAL_LEN],

            gc_index: 0,
            gc_regs: vec![0; GC_REGS_INITIAL_LEN],

            crtc_index: 0,
            crtc_regs: vec![0; CRTC_REGS_INITIAL_LEN],

            ac_index: 0,
            ac_regs: vec![0; AC_REGS_INITIAL_LEN],
            ac_flip_flop_data: Cell::new(false),
            ac_display_enabled: false,

            timing: VgaTiming::default(),

            vram: vec![0; 256 * 1024],

            legacy_bda: LegacyBdaInfo {
                video_mode: 0,
                columns: 0,
                rows: 0,
                page_size: 0,
                text_base_segment: 0,
                cursor_pos: [0; 8],
                active_page: 0,
            },

            derived: VgaDerivedState::default(),

            vbe: VbeState::new(),
            lfb: vec![0; VBE_LFB_SIZE],
            frontbuffer: Vec::new(),
            fb_dirty: false,
        };

        // Seed a sane "BIOS mode 3" (80x25 text) baseline. This matches the same
        // register tables used by `set_legacy_mode`, ensuring a single source of
        // truth for our default state.
        vga.set_legacy_mode(0x03, true);
        vga
    }

    fn reset_power_on(&mut self) {
        self.misc_output = POWER_ON_MISC_OUTPUT;
        self.video_subsystem_enable = 0x01;
        self.feature_control = 0x00;
        self.seq_index = 0;
        self.seq_regs.clear();
        self.seq_regs.resize(SEQ_REGS_INITIAL_LEN, 0);

        self.gc_index = 0;
        self.gc_regs.clear();
        self.gc_regs.resize(GC_REGS_INITIAL_LEN, 0);

        self.crtc_index = 0;
        self.crtc_regs.clear();
        self.crtc_regs.resize(CRTC_REGS_INITIAL_LEN, 0);

        self.ac_index = 0;
        self.ac_regs.clear();
        self.ac_regs.resize(AC_REGS_INITIAL_LEN, 0);

        self.ac_flip_flop_data.set(false);
        self.ac_display_enabled = false;
        self.timing = VgaTiming::default();

        self.vram.fill(0);
        self.legacy_bda = LegacyBdaInfo {
            video_mode: 0,
            columns: 0,
            rows: 0,
            page_size: 0,
            text_base_segment: 0,
            cursor_pos: [0; 8],
            active_page: 0,
        };
        self.vbe = VbeState::new();
        self.lfb.fill(0);
        self.frontbuffer.clear();
        self.fb_dirty = false;

        self.recompute_derived_state();
    }

    // --- VBE API surface (used by BIOS INT 10h handlers + unit tests) ---

    pub fn vbe(&self) -> &VbeState {
        &self.vbe
    }

    pub fn vbe_mut(&mut self) -> &mut VbeState {
        &mut self.vbe
    }

    pub fn controller_info(&self) -> VbeControllerInfo {
        self.vbe.controller_info()
    }

    pub fn mode_info(&self, mode: u16) -> Option<VbeModeInfo> {
        self.vbe.mode_info(mode)
    }

    /// Set a VBE mode (INT 10h AX=4F02 semantics).
    ///
    /// Bit 14 (`0x4000`) enables the linear framebuffer (LFB).
    pub fn set_mode(&mut self, mode: u16) -> Result<(), &'static str> {
        self.vbe.set_mode(mode)?;

        // Resize the host-visible buffer to the current mode.
        if let Some((w, h)) = self.vbe.resolution() {
            self.frontbuffer.resize(w as usize * h as usize, 0);
        } else {
            self.frontbuffer.clear();
        }

        self.fb_dirty = true;
        Ok(())
    }

    pub fn is_lfb_enabled(&self) -> bool {
        self.vbe.is_lfb_enabled()
    }

    pub fn resolution(&self) -> Option<(u16, u16)> {
        self.vbe.resolution()
    }

    pub fn pitch_bytes(&self) -> Option<u16> {
        self.vbe.pitch_bytes()
    }

    // --- Linear framebuffer + banked window backing store ---

    pub fn lfb_read(&self, offset: usize, dst: &mut [u8]) {
        let end = offset.saturating_add(dst.len()).min(self.lfb.len());
        let len = end.saturating_sub(offset);
        dst[..len].copy_from_slice(&self.lfb[offset..offset + len]);
        if len < dst.len() {
            dst[len..].fill(0);
        }
    }

    pub fn lfb_write(&mut self, offset: usize, src: &[u8]) {
        if !self.vbe.is_lfb_enabled() {
            // The LFB aperture is mapped but intentionally inert until enabled
            // via 4F02 (bit 14), matching real-world VBE implementations.
            return;
        }

        let end = offset.saturating_add(src.len()).min(self.lfb.len());
        let len = end.saturating_sub(offset);
        self.lfb[offset..offset + len].copy_from_slice(&src[..len]);
        if len > 0 {
            self.fb_dirty = true;
        }
    }

    pub fn banked_read(&self, offset: usize, dst: &mut [u8]) {
        let bank_base = (self.vbe.bank_a() as usize) * 64 * 1024;
        self.lfb_read(bank_base + offset, dst);
    }

    pub fn banked_write(&mut self, offset: usize, src: &[u8]) {
        let bank_base = (self.vbe.bank_a() as usize) * 64 * 1024;
        let end = (bank_base + offset)
            .saturating_add(src.len())
            .min(self.lfb.len());
        let len = end.saturating_sub(bank_base + offset);
        self.lfb[bank_base + offset..bank_base + offset + len].copy_from_slice(&src[..len]);
        if len > 0 {
            self.fb_dirty = true;
        }
    }

    /// Convert the guest framebuffer into a host-visible packed pixel buffer.
    ///
    /// - 32bpp: direct copy of little-endian `0xAARRGGBB` pixels.
    /// - 8bpp: palette lookup through the VGA palette stored in `VbeState`.
    pub fn render(&mut self) -> &[u32] {
        if !self.fb_dirty {
            return &self.frontbuffer;
        }
        self.fb_dirty = false;

        let Some(mode) = self.vbe.current_mode() else {
            self.frontbuffer.clear();
            return &self.frontbuffer;
        };

        let w = mode.width as usize;
        let h = mode.height as usize;
        let pitch = mode.pitch_bytes as usize;
        let bytes_per_pixel = mode.bytes_per_pixel as usize;

        self.frontbuffer.resize(w * h, 0);

        match mode.bits_per_pixel {
            32 => {
                let needed = pitch * h;
                let src = &self.lfb[..needed.min(self.lfb.len())];
                for y in 0..h {
                    let row = &src[y * pitch..(y + 1) * pitch];
                    for x in 0..w {
                        let base = x * bytes_per_pixel;
                        let px = u32::from_le_bytes([
                            row[base],
                            row[base + 1],
                            row[base + 2],
                            row[base + 3],
                        ]);
                        self.frontbuffer[y * w + x] = px;
                    }
                }
            }
            8 => {
                let palette = self.vbe.palette();
                let needed = pitch * h;
                let src = &self.lfb[..needed.min(self.lfb.len())];
                for y in 0..h {
                    let row = &src[y * pitch..(y + 1) * pitch];
                    for (x, &b) in row.iter().take(w).enumerate() {
                        let idx = b as usize;
                        self.frontbuffer[y * w + x] = palette[idx];
                    }
                }
            }
            _ => {
                self.frontbuffer.fill(0);
            }
        }

        &self.frontbuffer
    }

    pub fn tick(&mut self, delta_ns: u64) {
        self.timing.tick(delta_ns);
    }

    pub fn timing(&self) -> &VgaTiming {
        &self.timing
    }

    pub fn attribute_flip_flop_is_index(&self) -> bool {
        !self.ac_flip_flop_data.get()
    }

    pub fn should_render_text_attribute(&self, attribute: u8) -> bool {
        // Attribute controller Mode Control register: index 0x10, bit 3.
        let blink_enabled = (self.ac_regs.get(0x10).copied().unwrap_or(0) & 0x08) != 0;
        if blink_enabled && (attribute & 0x80) != 0 {
            return self.timing.text_blink_state_on();
        }
        true
    }

    fn is_colour_io(&self) -> bool {
        (self.misc_output & 0x01) != 0
    }

    fn active_crtc_ports(&self) -> (u16, u16) {
        if self.is_colour_io() {
            (PORT_CRTC_INDEX_COLOR, PORT_CRTC_DATA_COLOR)
        } else {
            (PORT_CRTC_INDEX_MONO, PORT_CRTC_DATA_MONO)
        }
    }

    fn active_input_status1_port(&self) -> u16 {
        if self.is_colour_io() {
            PORT_INPUT_STATUS1_COLOR
        } else {
            PORT_INPUT_STATUS1_MONO
        }
    }

    fn seq_reg_read(&self) -> u8 {
        self.seq_regs
            .get(self.seq_index as usize)
            .copied()
            .unwrap_or(0)
    }

    fn seq_reg_write(&mut self, val: u8) {
        let idx = self.seq_index as usize;
        if idx >= self.seq_regs.len() {
            self.seq_regs.resize(idx + 1, 0);
        }
        self.seq_regs[idx] = val;
        self.recompute_derived_state();
    }

    fn gc_reg_read(&self) -> u8 {
        self.gc_regs
            .get(self.gc_index as usize)
            .copied()
            .unwrap_or(0)
    }

    fn gc_reg_write(&mut self, val: u8) {
        let idx = self.gc_index as usize;
        if idx >= self.gc_regs.len() {
            self.gc_regs.resize(idx + 1, 0);
        }
        self.gc_regs[idx] = val;
        self.recompute_derived_state();
    }

    fn crtc_reg_read(&self) -> u8 {
        self.crtc_regs
            .get(self.crtc_index as usize)
            .copied()
            .unwrap_or(0)
    }

    fn crtc_reg_write(&mut self, val: u8) {
        let idx = self.crtc_index as usize;
        if idx >= self.crtc_regs.len() {
            self.crtc_regs.resize(idx + 1, 0);
        }
        if idx <= 0x07 {
            let protect = self.crtc_regs.get(0x11).copied().unwrap_or(0);
            if (protect & 0x80) != 0 {
                return;
            }
        }
        self.crtc_regs[idx] = val;
        self.recompute_derived_state();
    }

    fn ac_reg_read(&self) -> u8 {
        self.ac_regs
            .get(self.ac_index as usize)
            .copied()
            .unwrap_or(0)
    }

    fn ac_reg_write(&mut self, val: u8) {
        let idx = self.ac_index as usize;
        if idx >= self.ac_regs.len() {
            self.ac_regs.resize(idx + 1, 0);
        }
        self.ac_regs[idx] = val;
        self.recompute_derived_state();
    }

    fn recompute_derived_state(&mut self) {
        // Sequencer Memory Mode register: index 0x04.
        let seq_mem_mode = self.seq_regs.get(4).copied().unwrap_or(0);
        let chain4 = (seq_mem_mode & 0x08) != 0;

        // Graphics controller Mode register: index 0x05.
        let gc_mode = self.gc_regs.get(5).copied().unwrap_or(0);
        let seq_odd_even_disable = (seq_mem_mode & 0x04) != 0;
        let gc_odd_even_enable = (gc_mode & 0x10) != 0;
        let odd_even = (!seq_odd_even_disable) && gc_odd_even_enable;
        let shift_control = (gc_mode >> 5) & 0x03;
        let planar_shift = match shift_control {
            0 => VgaPlanarShift::None,
            1 => VgaPlanarShift::Shift256,
            _ => VgaPlanarShift::Interleaved,
        };

        // Graphics controller Miscellaneous register: index 0x06.
        let gc_misc = self.gc_regs.get(6).copied().unwrap_or(0);
        let gc_graphics = (gc_misc & 0x01) != 0;

        // Attribute controller Mode Control register: index 0x10.
        let ac_mode = self.ac_regs.get(0x10).copied().unwrap_or(0);
        let ac_graphics = (ac_mode & 0x01) != 0;

        let is_graphics = gc_graphics || ac_graphics;

        let bpp_guess = if !is_graphics {
            0
        } else if chain4 || matches!(planar_shift, VgaPlanarShift::Shift256) {
            8
        } else {
            4
        };

        let crtc_start_hi = self.crtc_regs.get(0x0C).copied().unwrap_or(0) as u32;
        let crtc_start_lo = self.crtc_regs.get(0x0D).copied().unwrap_or(0) as u32;
        let start_address = (crtc_start_hi << 8) | crtc_start_lo;

        let crtc_offset = self.crtc_regs.get(0x13).copied().unwrap_or(0) as u32;
        let crtc_underline = self.crtc_regs.get(0x14).copied().unwrap_or(0);
        let crtc_mode = self.crtc_regs.get(0x17).copied().unwrap_or(0);
        let mut pitch_bytes = crtc_offset.saturating_mul(2);
        if (crtc_mode & 0x40) == 0 {
            pitch_bytes = pitch_bytes.saturating_mul(2);
        }
        if (crtc_underline & 0x40) != 0 {
            pitch_bytes = pitch_bytes.saturating_mul(2);
        }

        let mem_map = (gc_misc >> 2) & 0x03;
        let (vram_window_base, vram_window_size) = match mem_map {
            0 => (0xA0000, 0x20000),
            1 => (0xA0000, 0x10000),
            2 => (0xB0000, 0x8000),
            3 => (0xB8000, 0x8000),
            _ => (0xA0000, 0),
        };

        let (width, height, text_columns, text_rows) = if !is_graphics {
            (0, 0, 80, 25)
        } else if chain4 || matches!(planar_shift, VgaPlanarShift::Shift256) {
            (320, 200, 0, 0)
        } else {
            (640, 480, 0, 0)
        };

        self.derived = VgaDerivedState {
            is_graphics,
            chain4,
            odd_even,
            planar_shift,
            bpp_guess,
            start_address,
            pitch_bytes,
            vram_window_base,
            vram_window_size,
            width,
            height,
            text_columns,
            text_rows,
        };
    }

    /// Input Status Register 1 (read via port 0x3DA on color adapters, 0x3BA on mono).
    ///
    /// Bit mapping implemented here:
    /// - Bit 3: vertical retrace / vblank (`1` while in vblank window).
    /// - Bit 0: display enable, inverted (`1` while display is disabled/blanked).
    fn read_input_status_1(&self) -> u8 {
        let mut v = 0u8;
        if self.timing.in_vblank() {
            v |= 1 << 3;
        }
        if !self.timing.display_enabled() {
            v |= 1 << 0;
        }
        v
    }

    fn read_u8(&self, port: u16) -> u8 {
        let (active_crtc_index, active_crtc_data) = self.active_crtc_ports();

        match port {
            PORT_MISC_OUTPUT_READ => self.misc_output,
            // Input Status 0 shares the 0x3C2 address with Misc Output writes.
            // Most software uses 0x3CC to read back misc_output; return a
            // conservative fixed value here.
            PORT_MISC_OUTPUT_WRITE => 0x00,
            PORT_VIDEO_SUBSYSTEM_ENABLE => self.video_subsystem_enable,
            PORT_FEATURE_CONTROL_READ => self.feature_control,

            PORT_SEQ_INDEX => self.seq_index,
            PORT_SEQ_DATA => self.seq_reg_read(),

            PORT_GC_INDEX => self.gc_index,
            PORT_GC_DATA => self.gc_reg_read(),

            PORT_AC_INDEX_DATA => {
                let display = if self.ac_display_enabled { 0x20 } else { 0x00 };
                display | (self.ac_index & 0x1F)
            }
            PORT_AC_DATA_READ => self.ac_reg_read(),

            p if p == self.active_input_status1_port() => {
                // Input Status 1 read resets the attribute controller flip-flop.
                self.ac_flip_flop_data.set(false);
                self.read_input_status_1()
            }

            p if p == active_crtc_index => self.crtc_index,
            p if p == active_crtc_data => self.crtc_reg_read(),

            // Unimplemented ports.
            _ => UNIMPLEMENTED_READ_VALUE,
        }
    }

    fn write_u8(&mut self, port: u16, val: u8) {
        if port != PORT_VIDEO_SUBSYSTEM_ENABLE && (self.video_subsystem_enable & 0x01) == 0 {
            return;
        }

        let (active_crtc_index, active_crtc_data) = self.active_crtc_ports();

        match port {
            PORT_VIDEO_SUBSYSTEM_ENABLE => self.video_subsystem_enable = val,
            PORT_MISC_OUTPUT_WRITE => {
                self.misc_output = val;
                self.recompute_derived_state();
            }

            PORT_SEQ_INDEX => self.seq_index = val,
            PORT_SEQ_DATA => self.seq_reg_write(val),

            PORT_GC_INDEX => self.gc_index = val,
            PORT_GC_DATA => self.gc_reg_write(val),

            PORT_AC_INDEX_DATA => {
                if !self.ac_flip_flop_data.get() {
                    // Index phase.
                    self.ac_display_enabled = (val & 0x20) != 0;
                    self.ac_index = val & 0x1F;
                    self.ac_flip_flop_data.set(true);
                } else {
                    // Data phase.
                    self.ac_reg_write(val);
                    self.ac_flip_flop_data.set(false);
                }
            }

            p if p == active_crtc_index => self.crtc_index = val,
            p if p == active_crtc_data => self.crtc_reg_write(val),

            p if p == self.active_input_status1_port() => {
                self.feature_control = val;
            }

            // Writes to unimplemented ports are ignored.
            _ => {}
        }
    }
}

impl PortIO for VgaDevice {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        match size {
            1 => u32::from(self.read_u8(port)),
            2 => {
                let lo = self.read_u8(port);
                let hi = self.read_u8(port.wrapping_add(1));
                u32::from(u16::from_le_bytes([lo, hi]))
            }
            4 => {
                let b0 = self.read_u8(port);
                let b1 = self.read_u8(port.wrapping_add(1));
                let b2 = self.read_u8(port.wrapping_add(2));
                let b3 = self.read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => 0,
        }
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        match size {
            1 => self.write_u8(port, val as u8),
            2 => {
                let [b0, b1] = (val as u16).to_le_bytes();
                self.write_u8(port, b0);
                self.write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = val.to_le_bytes();
                self.write_u8(port, b0);
                self.write_u8(port.wrapping_add(1), b1);
                self.write_u8(port.wrapping_add(2), b2);
                self.write_u8(port.wrapping_add(3), b3);
            }
            _ => {}
        }
    }
}
