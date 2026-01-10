use core::cell::Cell;

use crate::io::PortIO;

use super::{modeset, LegacyBdaInfo};
use super::regs::{
    AC_REGS_INITIAL_LEN, CRTC_REGS_INITIAL_LEN, GC_REGS_INITIAL_LEN, POWER_ON_MISC_OUTPUT,
    SEQ_REGS_INITIAL_LEN, VgaDerivedState, VgaPlanarShift,
};

// VGA I/O port block 0x3C0..=0x3DF.
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
const PORT_FEATURE_CONTROL_WRITE_COLOR: u16 = PORT_INPUT_STATUS1_COLOR;
const PORT_FEATURE_CONTROL_WRITE_MONO: u16 = PORT_INPUT_STATUS1_MONO;

const PORT_CRTC_INDEX_COLOR: u16 = 0x3D4;
const PORT_CRTC_DATA_COLOR: u16 = 0x3D5;
const PORT_CRTC_INDEX_MONO: u16 = 0x3B4;
const PORT_CRTC_DATA_MONO: u16 = 0x3B5;

/// Value returned from unimplemented reads in the 0x3C0..=0x3DF range.
///
/// Chosen to emulate a floating/pulled-up ISA bus (common behaviour for
/// unmapped ports).
const UNIMPLEMENTED_READ_VALUE: u8 = 0xFF;

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

    /// Simple, deterministic Input Status 1 vertical retrace bit generator.
    ///
    /// Real VGA hardware updates the vertical retrace bit based on scan timing.
    /// We do not model timing yet, but many real-mode programs poll 0x3DA
    /// waiting for the bit to change. If it never changes, they can spin
    /// forever. To avoid hangs while keeping behaviour predictable, we toggle
    /// the bit on each status read.
    input_status1_vretrace: Cell<bool>,

    /// 256 KiB VGA RAM, stored as four 64 KiB planes.
    vram: Vec<u8>,

    legacy_bda: LegacyBdaInfo,

    derived: VgaDerivedState,
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

        // Ensure the protected CRTC registers are unlocked while programming.
        if self.crtc_regs.len() > 0x11 {
            self.crtc_regs[0x03] |= 0x80;
            self.crtc_regs[0x11] &= !0x80;
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

            input_status1_vretrace: Cell::new(false),

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
        self.input_status1_vretrace.set(false);

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

        self.recompute_derived_state();
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
        let odd_even = (seq_mem_mode & 0x04) == 0;

        // Graphics controller Mode register: index 0x05.
        let gc_mode = self.gc_regs.get(5).copied().unwrap_or(0);
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

    fn read_u8(&self, port: u16) -> u8 {
        let (active_crtc_index, active_crtc_data) = self.active_crtc_ports();

        match port {
            PORT_MISC_OUTPUT_READ | PORT_MISC_OUTPUT_WRITE => self.misc_output,
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

            PORT_INPUT_STATUS1_COLOR | PORT_INPUT_STATUS1_MONO => {
                // Input Status 1 read resets the attribute controller flip-flop.
                self.ac_flip_flop_data.set(false);
                let next = !self.input_status1_vretrace.get();
                self.input_status1_vretrace.set(next);

                // Bit 3: vertical retrace (commonly polled).
                // Bit 0: display enable (roughly correlates with retrace/blanking).
                let v = if next { 0x08 } else { 0x00 };
                v | (v >> 3)
            }

            p if p == active_crtc_index => self.crtc_index,
            p if p == active_crtc_data => self.crtc_reg_read(),

            0x3CB | 0x3CD => 0x00,

            // Unimplemented ports.
            0x3C0..=0x3DF => UNIMPLEMENTED_READ_VALUE,
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

            PORT_FEATURE_CONTROL_WRITE_COLOR | PORT_FEATURE_CONTROL_WRITE_MONO => {
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
