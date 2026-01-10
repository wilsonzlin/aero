use core::cell::Cell;

use crate::io::PortIO;

use super::regs::{
    AC_REGS_INITIAL_LEN, BIOS_MODE3_AC_REGS, BIOS_MODE3_CRTC_REGS, BIOS_MODE3_GC_REGS,
    BIOS_MODE3_MISC_OUTPUT, BIOS_MODE3_SEQ_REGS, CRTC_REGS_INITIAL_LEN, GC_REGS_INITIAL_LEN,
    POWER_ON_MISC_OUTPUT, SEQ_REGS_INITIAL_LEN, VgaDerivedState, VgaPlanarShift,
};

// VGA I/O port block 0x3C0..=0x3DF.
const PORT_MISC_OUTPUT_WRITE: u16 = 0x3C2;
const PORT_MISC_OUTPUT_READ: u16 = 0x3CC;

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

/// Value returned from unimplemented reads in the 0x3C0..=0x3DF range.
///
/// Chosen to emulate a floating/pulled-up ISA bus (common behaviour for
/// unmapped ports).
const UNIMPLEMENTED_READ_VALUE: u8 = 0xFF;

pub struct VgaDevice {
    misc_output: u8,

    seq_index: u8,
    seq_regs: Vec<u8>,

    gc_index: u8,
    gc_regs: Vec<u8>,

    crtc_index: u8,
    crtc_regs: Vec<u8>,

    ac_index: u8,
    ac_regs: Vec<u8>,
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

    derived: VgaDerivedState,
}

impl Default for VgaDevice {
    fn default() -> Self {
        let mut vga = Self::new_with_bios_mode3_defaults();
        vga.recompute_derived_state();
        vga
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

    fn new_with_bios_mode3_defaults() -> Self {
        Self {
            // VGA Misc Output Register bit 0 selects the I/O base:
            // - 0 = 0x3Bx (mono)
            // - 1 = 0x3Dx (colour)
            misc_output: BIOS_MODE3_MISC_OUTPUT,

            seq_index: 0,
            seq_regs: BIOS_MODE3_SEQ_REGS.to_vec(),

            gc_index: 0,
            gc_regs: BIOS_MODE3_GC_REGS.to_vec(),

            crtc_index: 0,
            crtc_regs: BIOS_MODE3_CRTC_REGS.to_vec(),

            ac_index: 0,
            ac_regs: BIOS_MODE3_AC_REGS.to_vec(),
            ac_flip_flop_data: Cell::new(false),
            ac_display_enabled: true,

            input_status1_vretrace: Cell::new(false),

            derived: VgaDerivedState::default(),
        }
    }

    fn reset_power_on(&mut self) {
        self.misc_output = POWER_ON_MISC_OUTPUT;
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

        self.derived = VgaDerivedState {
            is_graphics,
            chain4,
            odd_even,
            planar_shift,
            bpp_guess,
        };
    }

    fn read_u8(&self, port: u16) -> u8 {
        let (active_crtc_index, active_crtc_data) = self.active_crtc_ports();

        match port {
            PORT_MISC_OUTPUT_READ => self.misc_output,

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
                let next = !self.input_status1_vretrace.get();
                self.input_status1_vretrace.set(next);

                // Bit 3: vertical retrace (commonly polled).
                // Bit 0: display enable (roughly correlates with retrace/blanking).
                let v = if next { 0x08 } else { 0x00 };
                v | (v >> 3)
            }

            p if p == active_crtc_index => self.crtc_index,
            p if p == active_crtc_data => self.crtc_reg_read(),

            // Unimplemented ports.
            0x3C0..=0x3DF => UNIMPLEMENTED_READ_VALUE,
            _ => UNIMPLEMENTED_READ_VALUE,
        }
    }

    fn write_u8(&mut self, port: u16, val: u8) {
        let (active_crtc_index, active_crtc_data) = self.active_crtc_ports();

        match port {
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
