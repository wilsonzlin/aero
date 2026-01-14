//! Intel ICH AC'97 controller emulation (minimal playback-only).
//!
//! The controller provides two I/O regions:
//! - NAM (Native Audio Mixer): AC'97 codec register space
//! - NABM (Native Audio Bus Master): DMA engine + global control/status
//!
//! This module implements a narrow subset needed as a fallback audio path
//! when full HD Audio emulation is unavailable.

pub mod dma;
pub mod regs;

use memory::MemoryBus;

use crate::io::pci::{PciConfigSpace, PciDevice};
use aero_platform::io::PortIoDevice;

use crate::io::audio::ac97::dma::{AudioSink, PcmOutDma};
use crate::io::audio::ac97::regs::*;

/// PCI IDs for the Intel 82801AA (ICH) AC'97 controller.
pub const PCI_VENDOR_ID_INTEL: u16 = 0x8086;
pub const PCI_DEVICE_ID_ICH_AC97: u16 = 0x2415;

pub const NAM_SIZE: u16 = 0x100;
pub const NABM_SIZE: u16 = 0x100;

#[derive(Debug, Clone)]
struct MixerRegs {
    regs: [u16; (NAM_SIZE as usize) / 2],
}

impl MixerRegs {
    fn new() -> Self {
        let mut this = Self {
            regs: [0; (NAM_SIZE as usize) / 2],
        };
        this.reset();
        this
    }

    fn reset(&mut self) {
        self.regs = [0; (NAM_SIZE as usize) / 2];

        self.write_u16(NAM_MASTER_VOL, 0x0000);
        self.write_u16(NAM_PCM_OUT_VOL, 0x0000);

        // Report basic capabilities: Variable Rate Audio supported.
        self.write_u16(NAM_EXT_AUDIO_ID, 0x0001);
        self.write_u16(NAM_EXT_AUDIO_CTRL, 0x0001);
        self.write_u16(NAM_PCM_FRONT_DAC_RATE, 48_000);

        // Match QEMU's default codec IDs (SigmaTel STAC9700).
        self.write_u16(NAM_VENDOR_ID1, 0x8384);
        self.write_u16(NAM_VENDOR_ID2, 0x7600);
    }

    fn read_u16(&self, offset: u64) -> u16 {
        let idx = (offset / 2) as usize;
        self.regs.get(idx).copied().unwrap_or(0)
    }

    fn write_u16(&mut self, offset: u64, value: u16) {
        let idx = (offset / 2) as usize;
        if let Some(slot) = self.regs.get_mut(idx) {
            *slot = value;
        }
    }
}

/// AC'97 controller core state (codec + PCM out DMA engine).
#[derive(Debug, Clone)]
pub struct Ac97Controller {
    mixer: MixerRegs,
    pub pcm_out: PcmOutDma,

    glob_cnt: u32,
    glob_sta: u32,
    acc_sema: u8,
    irq_level: bool,
}

impl Default for Ac97Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl Ac97Controller {
    pub fn new() -> Self {
        let mut this = Self {
            mixer: MixerRegs::new(),
            pcm_out: PcmOutDma::default(),
            glob_cnt: GLOB_CNT_GIE,
            // Mark codec 0 as ready in multiple bit positions to accommodate
            // differing expectations across drivers.
            glob_sta: 0x0000_0001 | 0x0000_0100,
            acc_sema: 0x01,
            irq_level: false,
        };
        this.reset();
        this
    }

    pub fn reset(&mut self) {
        self.mixer.reset();
        self.pcm_out = PcmOutDma::default();
        self.glob_cnt = GLOB_CNT_GIE;
        self.glob_sta = 0x0000_0001 | 0x0000_0100;
        self.acc_sema = 0x01;
        self.update_irq_level();
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    fn global_interrupts_enabled(&self) -> bool {
        (self.glob_cnt & GLOB_CNT_GIE) != 0
    }

    fn update_irq_level(&mut self) {
        self.irq_level = self.global_interrupts_enabled() && self.pcm_out.irq_pending();
    }

    /// Progress the PCM-out DMA engine and update IRQ status.
    pub fn poll(&mut self, mem: &mut dyn MemoryBus, out: &mut impl AudioSink, max_words: u16) {
        self.pcm_out.tick(mem, out, max_words);
        self.update_irq_level();
    }

    /// Read from NAM (mixer) register space.
    pub fn nam_read(&self, offset: u64, size: usize) -> u32 {
        match size {
            4 => {
                // Some guests probe the codec using 32-bit I/O accesses. Treat this as two
                // consecutive 16-bit register reads, packed little-endian.
                //
                // Only accept 2-byte aligned accesses that stay within the NAM window; anything
                // else returns 0 (conservative, matches previous unsupported-size behavior).
                if (offset & 1) != 0 {
                    return 0;
                }
                let end = match offset.checked_add(4) {
                    Some(end) => end,
                    None => return 0,
                };
                if end > u64::from(NAM_SIZE) {
                    return 0;
                }

                let low = self.mixer.read_u16(offset);
                let high = self.mixer.read_u16(offset + 2);
                u32::from(low) | (u32::from(high) << 16)
            }
            2 => self.mixer.read_u16(offset & !1) as u32,
            1 => {
                let word = self.mixer.read_u16(offset & !1);
                if (offset & 1) == 0 {
                    (word & 0x00FF) as u32
                } else {
                    (word >> 8) as u32
                }
            }
            _ => 0,
        }
    }

    /// Write to NAM (mixer) register space.
    pub fn nam_write(&mut self, offset: u64, size: usize, value: u32) {
        match (offset, size) {
            (NAM_RESET, 2) => {
                let _ = value;
                self.mixer.reset();
            }
            (_, 4) => {
                // Treat 32-bit accesses as two 16-bit register writes, packed little-endian.
                // This matches how some guests access the AC'97 NAM register file.
                if (offset & 1) != 0 {
                    return;
                }
                let end = match offset.checked_add(4) {
                    Some(end) => end,
                    None => return,
                };
                if end > u64::from(NAM_SIZE) {
                    return;
                }

                let low = value as u16;
                let high = (value >> 16) as u16;

                // Preserve the special reset behaviour when the low half targets NAM_RESET.
                if offset == NAM_RESET {
                    let _ = low;
                    self.mixer.reset();
                } else {
                    self.mixer.write_u16(offset, low);
                }
                self.mixer.write_u16(offset + 2, high);
            }
            (_, 2) => self.mixer.write_u16(offset & !1, value as u16),
            (_, 1) => {
                // Read-modify-write for byte accesses.
                let off = offset & !1;
                let mut word = self.mixer.read_u16(off);
                if (offset & 1) == 0 {
                    word = (word & 0xFF00) | ((value as u16) & 0x00FF);
                } else {
                    word = (word & 0x00FF) | (((value as u16) & 0x00FF) << 8);
                }
                self.mixer.write_u16(off, word);
            }
            _ => {}
        }
    }

    /// Read from NABM (bus master) register space.
    pub fn nabm_read(&self, offset: u64, size: usize) -> u32 {
        match size {
            1 => u32::from(self.nabm_read_u8(offset)),
            2 => {
                let b0 = u32::from(self.nabm_read_u8(offset));
                let b1 = u32::from(self.nabm_read_u8(offset.wrapping_add(1)));
                b0 | (b1 << 8)
            }
            4 => {
                let mut out = 0u32;
                for i in 0..4u64 {
                    out |= u32::from(self.nabm_read_u8(offset.wrapping_add(i))) << (i * 8);
                }
                out
            }
            _ => 0,
        }
    }

    /// Write to NABM (bus master) register space.
    pub fn nabm_write(&mut self, offset: u64, size: usize, value: u32) {
        let mut did_reset = false;
        match size {
            1 => {
                did_reset = self.nabm_write_u8(offset, value as u8);
            }
            2 => {
                for i in 0..2u64 {
                    let byte = ((value >> (i * 8)) & 0xff) as u8;
                    did_reset = self.nabm_write_u8(offset.wrapping_add(i), byte);
                    if did_reset {
                        break;
                    }
                }
            }
            4 => {
                for i in 0..4u64 {
                    let byte = ((value >> (i * 8)) & 0xff) as u8;
                    did_reset = self.nabm_write_u8(offset.wrapping_add(i), byte);
                    if did_reset {
                        break;
                    }
                }
            }
            _ => {}
        }
        if !did_reset {
            self.update_irq_level();
        }
    }

    fn nabm_read_u8(&self, offset: u64) -> u8 {
        const PO_BDBAR_END: u64 = NABM_PO_BDBAR + 3;
        const PO_SR_HI: u64 = NABM_PO_SR + 1;
        const PO_PICB_HI: u64 = NABM_PO_PICB + 1;
        const GLOB_CNT_END: u64 = NABM_GLOB_CNT + 3;
        const GLOB_STA_END: u64 = NABM_GLOB_STA + 3;

        match offset {
            // PCM out stream registers.
            NABM_PO_BDBAR..=PO_BDBAR_END => {
                let shift = (offset - NABM_PO_BDBAR) * 8;
                ((self.pcm_out.bdbar >> shift) & 0xff) as u8
            }
            NABM_PO_CIV => self.pcm_out.civ(),
            NABM_PO_LVI => self.pcm_out.lvi(),
            NABM_PO_SR => (self.pcm_out.sr() & 0x00ff) as u8,
            PO_SR_HI => (self.pcm_out.sr() >> 8) as u8,
            NABM_PO_PICB => (self.pcm_out.picb() & 0x00ff) as u8,
            PO_PICB_HI => (self.pcm_out.picb() >> 8) as u8,
            NABM_PO_PIV => self.pcm_out.piv(),
            NABM_PO_CR => self.pcm_out.cr(),

            // Global registers.
            NABM_GLOB_CNT..=GLOB_CNT_END => {
                let shift = (offset - NABM_GLOB_CNT) * 8;
                ((self.glob_cnt >> shift) & 0xff) as u8
            }
            NABM_GLOB_STA..=GLOB_STA_END => {
                let shift = (offset - NABM_GLOB_STA) * 8;
                ((self.glob_sta >> shift) & 0xff) as u8
            }
            NABM_ACC_SEMA => self.acc_sema,

            _ => 0,
        }
    }

    /// Returns `true` if the write triggers a full controller reset.
    fn nabm_write_u8(&mut self, offset: u64, value: u8) -> bool {
        const PO_BDBAR_END: u64 = NABM_PO_BDBAR + 3;
        const PO_SR_HI: u64 = NABM_PO_SR + 1;
        const GLOB_CNT_END: u64 = NABM_GLOB_CNT + 3;

        match offset {
            // PCM out stream registers.
            NABM_PO_BDBAR..=PO_BDBAR_END => {
                let shift = (offset - NABM_PO_BDBAR) * 8;
                let mask = !(0xffu32 << shift);
                let next = (self.pcm_out.bdbar & mask) | (u32::from(value) << shift);
                self.pcm_out.write_bdbar(next);
            }
            NABM_PO_LVI => self.pcm_out.write_lvi(value),
            NABM_PO_SR => self.pcm_out.write_sr(u16::from(value)),
            PO_SR_HI => self.pcm_out.write_sr(u16::from(value) << 8),
            NABM_PO_CR => self.pcm_out.write_cr(value),

            // Global registers.
            NABM_GLOB_CNT..=GLOB_CNT_END => {
                let shift = (offset - NABM_GLOB_CNT) * 8;
                let mask = !(0xffu32 << shift);
                self.glob_cnt = (self.glob_cnt & mask) | (u32::from(value) << shift);

                if (self.glob_cnt & (GLOB_CNT_COLD_RESET | GLOB_CNT_WARM_RESET)) != 0 {
                    self.reset();
                    return true;
                }
            }
            NABM_ACC_SEMA => {
                let _ = value;
                // Writes are ignored; semaphore always immediately available.
                self.acc_sema = 0x01;
            }

            _ => {}
        }

        false
    }
}

/// PCI wrapper that exposes the AC'97 controller as an Intel ICH-compatible device.
#[derive(Debug)]
pub struct Ac97PciDevice {
    config: PciConfigSpace,
    bar_nam: u16,
    bar_nabm: u16,
    bar0_probe: bool,
    bar1_probe: bool,

    pub controller: Ac97Controller,
}

impl Ac97PciDevice {
    pub fn new(nam_base: u16, nabm_base: u16) -> Self {
        let mut config = PciConfigSpace::new();

        config.set_u16(0x00, PCI_VENDOR_ID_INTEL);
        config.set_u16(0x02, PCI_DEVICE_ID_ICH_AC97);

        // Class code: multimedia / audio controller.
        config.write(0x0a, 1, 0x01); // subclass
        config.write(0x0b, 1, 0x04); // class

        // BAR0 (NAM) + BAR1 (NABM) are I/O space.
        let nam_base = nam_base & !(NAM_SIZE - 1);
        let nabm_base = nabm_base & !(NABM_SIZE - 1);
        config.set_u32(0x10, u32::from(nam_base) | 0x1);
        config.set_u32(0x14, u32::from(nabm_base) | 0x1);

        // INTA#
        config.set_u8(0x3d, 1);

        Self {
            config,
            bar_nam: nam_base,
            bar_nabm: nabm_base,
            bar0_probe: false,
            bar1_probe: false,
            controller: Ac97Controller::new(),
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn io_space_enabled(&self) -> bool {
        (self.command() & (1 << 0)) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        (self.command() & (1 << 2)) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.controller.irq_level()
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus, out: &mut impl AudioSink, max_words: u16) {
        // Gate DMA on PCI command Bus Master Enable (bit 2).
        //
        // AC'97 bus mastering reads descriptors + audio buffers from guest memory. When the guest
        // clears COMMAND.BME, the device must not perform DMA.
        if !self.bus_master_enabled() {
            return;
        }
        self.controller.poll(mem, out, max_words);
    }

    fn port_to_nam_offset(&self, port: u16) -> Option<u64> {
        // BARs in PCI config space are 32-bit, but x86 port space is 16-bit. When the BAR base
        // is near 0xFFFF, `base + size` can overflow a u16 and erroneously make the decoded
        // range empty. Use wider arithmetic so a base like 0xFF00 still decodes 0xFF00..0xFFFF.
        let base = u32::from(self.bar_nam);
        let port = u32::from(port);
        let end = base + u32::from(NAM_SIZE);
        if port >= base && port < end {
            Some(u64::from(port - base))
        } else {
            None
        }
    }

    fn port_to_nabm_offset(&self, port: u16) -> Option<u64> {
        let base = u32::from(self.bar_nabm);
        let port = u32::from(port);
        let end = base + u32::from(NABM_SIZE);
        if port >= base && port < end {
            Some(u64::from(port - base))
        } else {
            None
        }
    }
}

impl PciDevice for Ac97PciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }
        let Some(end) = offset.checked_add(size as u16) else {
            return 0;
        };
        if end as usize > 256 {
            return 0;
        }

        let bar0_off = 0x10u16;
        let bar0_end = bar0_off + 4;
        let bar1_off = 0x14u16;
        let bar1_end = bar1_off + 4;

        let overlaps_bar0 = offset < bar0_end && end > bar0_off;
        let overlaps_bar1 = offset < bar1_end && end > bar1_off;

        if overlaps_bar0 || overlaps_bar1 {
            let bar0_mask = (!(u32::from(NAM_SIZE) - 1) & 0xffff_fffc) | 0x1;
            let bar0_val = if self.bar0_probe {
                bar0_mask
            } else {
                u32::from(self.bar_nam) | 0x1
            };

            let bar1_mask = (!(u32::from(NABM_SIZE) - 1) & 0xffff_fffc) | 0x1;
            let bar1_val = if self.bar1_probe {
                bar1_mask
            } else {
                u32::from(self.bar_nabm) | 0x1
            };

            let mut out = 0u32;
            for i in 0..size {
                let byte_off = offset + i as u16;
                let byte = if (bar0_off..bar0_end).contains(&byte_off) {
                    let shift = u32::from(byte_off - bar0_off) * 8;
                    (bar0_val >> shift) & 0xFF
                } else if (bar1_off..bar1_end).contains(&byte_off) {
                    let shift = u32::from(byte_off - bar1_off) * 8;
                    (bar1_val >> shift) & 0xFF
                } else {
                    self.config.read(byte_off, 1) & 0xFF
                };
                out |= byte << (8 * i);
            }
            return out;
        }

        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let Some(end) = offset.checked_add(size as u16) else {
            return;
        };
        if end as usize > 256 {
            return;
        }

        let bar0_off = 0x10u16;
        let bar0_end = bar0_off + 4;
        let bar1_off = 0x14u16;
        let bar1_end = bar1_off + 4;

        let overlaps_bar0 = offset < bar0_end && end > bar0_off;
        let overlaps_bar1 = offset < bar1_end && end > bar1_off;

        // PCI BAR probing uses an all-ones write to discover the size mask.
        if offset == bar0_off && size == 4 && value == 0xffff_ffff {
            self.bar0_probe = true;
            return;
        }
        if offset == bar1_off && size == 4 && value == 0xffff_ffff {
            self.bar1_probe = true;
            return;
        }

        if overlaps_bar0 || overlaps_bar1 {
            if overlaps_bar0 {
                self.bar0_probe = false;
            }
            if overlaps_bar1 {
                self.bar1_probe = false;
            }

            self.config.write(offset, size, value);

            if overlaps_bar0 {
                let raw = self.config.read(bar0_off, 4);
                let addr_mask = !(u32::from(NAM_SIZE) - 1) & 0xffff_fffc;
                let base = raw & addr_mask;
                self.bar_nam = u16::try_from(base).unwrap_or(u16::MAX);
                self.config
                    .set_u32(bar0_off as usize, u32::from(self.bar_nam) | 0x1);
            }

            if overlaps_bar1 {
                let raw = self.config.read(bar1_off, 4);
                let addr_mask = !(u32::from(NABM_SIZE) - 1) & 0xffff_fffc;
                let base = raw & addr_mask;
                self.bar_nabm = u16::try_from(base).unwrap_or(u16::MAX);
                self.config
                    .set_u32(bar1_off as usize, u32::from(self.bar_nabm) | 0x1);
            }

            return;
        }

        self.config.write(offset, size, value);
    }
}

impl PortIoDevice for Ac97PciDevice {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let size_usize = match size {
            0 => return 0,
            1 | 2 | 4 => size as usize,
            _ => return u32::MAX,
        };
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return match size_usize {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        }
        if let Some(off) = self.port_to_nam_offset(port) {
            return self.controller.nam_read(off, size_usize);
        }
        if let Some(off) = self.port_to_nabm_offset(port) {
            return self.controller.nabm_read(off, size_usize);
        }
        0
    }

    fn write(&mut self, port: u16, size: u8, val: u32) {
        let size_usize = match size {
            0 => return,
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return;
        }
        if let Some(off) = self.port_to_nam_offset(port) {
            self.controller.nam_write(off, size_usize, val);
            return;
        }
        if let Some(off) = self.port_to_nabm_offset(port) {
            self.controller.nabm_write(off, size_usize, val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_platform::io::PortIoDevice;
    use memory::Bus;

    #[derive(Default)]
    struct NullSink;

    impl AudioSink for NullSink {
        fn push_interleaved_f32(&mut self, _samples: &[f32]) {}
    }

    fn program_single_descriptor(
        mem: &mut Bus,
        bdbar: u64,
        buf_addr: u32,
        len_words: u16,
        ioc: bool,
    ) {
        mem.write_u32(bdbar, buf_addr);
        let mut ctl = u32::from(len_words);
        if ioc {
            ctl |= BDL_IOC;
        }
        mem.write_u32(bdbar + 4, ctl);
    }

    fn controller_with_sr_irq_bits_set() -> Ac97Controller {
        let mut ctrl = Ac97Controller::new();
        let mut mem = Bus::new(0x4000);

        let bdl_addr = 0x1000u64;
        program_single_descriptor(&mut mem, bdl_addr, 0x2000, 0, true);

        ctrl.nabm_write(NABM_PO_BDBAR, 4, bdl_addr as u32);
        ctrl.nabm_write(NABM_PO_LVI, 1, 0);
        ctrl.nabm_write(NABM_PO_CR, 1, u32::from(CR_RPBM));

        let mut sink = NullSink;
        ctrl.poll(&mut mem, &mut sink, 0);

        let sr = ctrl.nabm_read(NABM_PO_SR, 2) as u16;
        assert_eq!(sr & (SR_BCIS | SR_LVBCI), SR_BCIS | SR_LVBCI);

        ctrl
    }

    #[test]
    fn mixer_vendor_ids_are_exposed() {
        let dev = Ac97Controller::new();
        assert_eq!(dev.nam_read(NAM_VENDOR_ID1, 2) as u16, 0x8384);
        assert_eq!(dev.nam_read(NAM_VENDOR_ID2, 2) as u16, 0x7600);
    }

    #[test]
    fn mixer_volume_roundtrips_and_reset_restores_defaults() {
        let mut dev = Ac97Controller::new();

        dev.nam_write(NAM_MASTER_VOL, 2, 0x1234);
        assert_eq!(dev.nam_read(NAM_MASTER_VOL, 2) as u16, 0x1234);

        dev.nam_write(NAM_RESET, 2, 0);
        assert_eq!(dev.nam_read(NAM_MASTER_VOL, 2) as u16, 0x0000);
    }

    #[test]
    fn mixer_supports_32bit_register_accesses() {
        let mut dev = Ac97Controller::new();

        dev.nam_write(NAM_MASTER_VOL, 4, 0x7654_3210);
        assert_eq!(dev.nam_read(NAM_MASTER_VOL, 4), 0x7654_3210);
        assert_eq!(dev.nam_read(NAM_MASTER_VOL, 2) as u16, 0x3210);
        assert_eq!(dev.nam_read(NAM_MASTER_VOL + 2, 2) as u16, 0x7654);
    }

    #[test]
    fn mixer_vendor_ids_can_be_read_as_packed_dword() {
        let dev = Ac97Controller::new();
        assert_eq!(dev.nam_read(NAM_VENDOR_ID1, 4), 0x7600_8384);
    }

    #[test]
    fn nabm_word_reads_pack_civ_and_lvi() {
        let mut ctrl = Ac97Controller::new();
        let mut mem = Bus::new(0x4000);

        let bdl_addr = 0x1000u64;
        // Use a zero-length buffer so the DMA engine completes immediately and increments CIV.
        program_single_descriptor(&mut mem, bdl_addr, 0x2000, 0, false);

        ctrl.nabm_write(NABM_PO_BDBAR, 4, bdl_addr as u32);
        ctrl.nabm_write(NABM_PO_LVI, 1, 0);
        ctrl.nabm_write(NABM_PO_CR, 1, u32::from(CR_RPBM));

        let mut sink = NullSink;
        ctrl.poll(&mut mem, &mut sink, 0);

        // Program LVI after the engine has advanced CIV to ensure both bytes are non-zero.
        ctrl.nabm_write(NABM_PO_LVI, 1, 5);

        let got = ctrl.nabm_read(NABM_PO_CIV, 2) as u16;
        assert_eq!(got, u16::from_le_bytes([1, 5]));
    }

    #[test]
    fn nabm_dword_reads_pack_stream_regs_little_endian() {
        let mut ctrl = Ac97Controller::new();
        let mut mem = Bus::new(0x4000);

        let bdl_addr = 0x1000u64;
        // Use IOC+zero-length so SR has known W1C bits set and CIV advances.
        program_single_descriptor(&mut mem, bdl_addr, 0x2000, 0, true);

        ctrl.nabm_write(NABM_PO_BDBAR, 4, bdl_addr as u32);
        ctrl.nabm_write(NABM_PO_LVI, 1, 0);
        ctrl.nabm_write(NABM_PO_CR, 1, u32::from(CR_RPBM));

        let mut sink = NullSink;
        ctrl.poll(&mut mem, &mut sink, 0);

        // Distinct LVI makes byte ordering obvious.
        ctrl.nabm_write(NABM_PO_LVI, 1, 5);

        // 32-bit read at CIV spans CIV/LVI/SR (2 bytes).
        let got = ctrl.nabm_read(NABM_PO_CIV, 4);
        let expected_sr = SR_BCIS | SR_LVBCI | SR_DCH;
        let expected =
            u32::from_le_bytes([1, 5, (expected_sr & 0xff) as u8, (expected_sr >> 8) as u8]);
        assert_eq!(got, expected);
    }

    #[test]
    fn nabm_status_w1c_works_for_1_2_and_4_byte_writes() {
        // 1-byte write to SR low byte.
        {
            let mut ctrl = controller_with_sr_irq_bits_set();
            ctrl.nabm_write(NABM_PO_SR, 1, u32::from(SR_BCIS as u8));
            let sr = ctrl.nabm_read(NABM_PO_SR, 2) as u16;
            assert_eq!(sr & SR_BCIS, 0);
            assert_ne!(sr & SR_LVBCI, 0);
        }

        // 2-byte write to SR (natural width).
        {
            let mut ctrl = controller_with_sr_irq_bits_set();
            ctrl.nabm_write(NABM_PO_SR, 2, u32::from(SR_BCIS | SR_LVBCI));
            let sr = ctrl.nabm_read(NABM_PO_SR, 2) as u16;
            assert_eq!(sr & (SR_BCIS | SR_LVBCI), 0);
        }

        // 4-byte write starting at CIV, spanning into SR.
        {
            let mut ctrl = controller_with_sr_irq_bits_set();
            ctrl.nabm_write(NABM_PO_CIV, 4, u32::from(SR_BCIS | SR_LVBCI) << 16);
            let sr = ctrl.nabm_read(NABM_PO_SR, 2) as u16;
            assert_eq!(sr & (SR_BCIS | SR_LVBCI), 0);
        }
    }

    #[test]
    fn pci_wrapper_gates_ac97_ports_on_pci_command_io_bit() {
        let mut dev = Ac97PciDevice::new(0x1000, 0x1100);

        // With COMMAND.IO clear, reads float high and writes are ignored.
        let vid1_port = 0x1000u16 + NAM_VENDOR_ID1 as u16;
        assert_eq!(dev.read(vid1_port, 2), 0xffff);

        let master_vol_port = 0x1000u16 + NAM_MASTER_VOL as u16;
        dev.write(master_vol_port, 2, 0x1234);

        // Enable IO decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 0);
        assert_eq!(dev.read(vid1_port, 2) as u16, 0x8384);
        assert_eq!(dev.read(master_vol_port, 2) as u16, 0x0000);
    }

    #[test]
    fn pci_wrapper_masks_io_bar_bases_to_bar_size() {
        let mut dev = Ac97PciDevice::new(0x1234, 0x5678);

        // Initial BAR values should be masked to the BAR size alignment.
        assert_eq!(dev.config_read(0x10, 4), 0x1201);
        assert_eq!(dev.config_read(0x14, 4), 0x5601);

        dev.config_write(0x10, 4, 0xffff_ffff);
        let mask0 = dev.config_read(0x10, 4);
        dev.config_write(0x10, 4, 0x1235);
        let addr_mask0 = mask0 & !0x3;
        let flags0 = mask0 & 0x3;
        assert_eq!(dev.config_read(0x10, 4), (0x1235 & addr_mask0) | flags0);
        assert_eq!(dev.bar_nam, (0x1235 & addr_mask0) as u16);

        dev.config_write(0x14, 4, 0xffff_ffff);
        let mask1 = dev.config_read(0x14, 4);
        dev.config_write(0x14, 4, 0x5679);
        let addr_mask1 = mask1 & !0x3;
        let flags1 = mask1 & 0x3;
        assert_eq!(dev.config_read(0x14, 4), (0x5679 & addr_mask1) | flags1);
        assert_eq!(dev.bar_nabm, (0x5679 & addr_mask1) as u16);
    }

    #[test]
    fn pci_bar_probe_subword_reads_return_mask_bytes() {
        let mut dev = Ac97PciDevice::new(0x1234, 0x5678);

        dev.config_write(0x10, 4, 0xffff_ffff);
        let mask0 = dev.config_read(0x10, 4);
        assert_eq!(mask0, (!(u32::from(NAM_SIZE) - 1) & 0xffff_fffc) | 0x1);
        assert_eq!(dev.config_read(0x10, 1), mask0 & 0xFF);
        assert_eq!(dev.config_read(0x11, 1), (mask0 >> 8) & 0xFF);
        assert_eq!(dev.config_read(0x12, 2), (mask0 >> 16) & 0xFFFF);

        dev.config_write(0x14, 4, 0xffff_ffff);
        let mask1 = dev.config_read(0x14, 4);
        assert_eq!(mask1, (!(u32::from(NABM_SIZE) - 1) & 0xffff_fffc) | 0x1);
        assert_eq!(dev.config_read(0x14, 1), mask1 & 0xFF);
        assert_eq!(dev.config_read(0x15, 1), (mask1 >> 8) & 0xFF);
        assert_eq!(dev.config_read(0x16, 2), (mask1 >> 16) & 0xFFFF);
    }

    #[test]
    fn pci_bar_subword_write_updates_io_bases() {
        let mut dev = Ac97PciDevice::new(0, 0);

        dev.config_write(0x10, 2, 0x1235);
        assert_eq!(dev.bar_nam, 0x1200);
        assert_eq!(dev.config_read(0x10, 4), 0x1201);

        dev.config_write(0x14, 2, 0x5679);
        assert_eq!(dev.bar_nabm, 0x5600);
        assert_eq!(dev.config_read(0x14, 4), 0x5601);
    }

    #[test]
    fn pci_wrapper_decodes_ports_when_io_bars_are_near_u16_max() {
        // Regression test: when BAR base + size overflows a u16, the device should still decode
        // the portion of the range that exists in x86's 16-bit I/O port space.

        // NAM at 0xFF00 would wrap on `0xFF00u16.wrapping_add(0x100) == 0x0000`.
        let mut dev = Ac97PciDevice::new(0xFF00, 0xFE00);
        dev.config_write(0x04, 2, 1 << 0);

        let vid1_port = 0xFF00u16 + NAM_VENDOR_ID1 as u16;
        let vid2_port = 0xFF00u16 + NAM_VENDOR_ID2 as u16;
        assert_eq!(dev.read(vid1_port, 2) as u16, 0x8384);
        assert_eq!(dev.read(vid2_port, 2) as u16, 0x7600);

        // Also cover the NABM path with a BAR that would overflow.
        let mut dev = Ac97PciDevice::new(0xFE00, 0xFF00);
        dev.config_write(0x04, 2, 1 << 0);

        let civ_port = 0xFF00u16 + NABM_PO_CIV as u16;
        assert_eq!(dev.read(civ_port, 1) as u8, 0);
    }

    #[test]
    fn pci_wrapper_gates_ac97_dma_on_pci_command_bme_bit() {
        struct PanicMem;

        impl MemoryBus for PanicMem {
            fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
                panic!("unexpected DMA read");
            }

            fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
                panic!("unexpected DMA write");
            }
        }

        #[derive(Default)]
        struct Sink;

        impl AudioSink for Sink {
            fn push_interleaved_f32(&mut self, _samples: &[f32]) {}
        }

        let mut dev = Ac97PciDevice::new(0x1000, 0x1100);
        // Enable I/O decoding so we can program the NABM registers, but leave BME disabled.
        dev.config_write(0x04, 2, 1 << 0);

        // Start the PCM out DMA engine (this would DMA immediately if BME was enabled).
        dev.write(0x1100 + NABM_PO_BDBAR as u16, 4, 0x2000);
        dev.write(0x1100 + NABM_PO_LVI as u16, 1, 0);
        dev.write(0x1100 + NABM_PO_CR as u16, 1, u32::from(CR_RPBM));

        let mut mem = PanicMem;
        let mut sink = Sink;

        // With BME clear, wrapper.poll must not touch guest memory.
        dev.poll(&mut mem, &mut sink, 2);

        // Enable bus mastering and verify the DMA engine attempts a memory access.
        dev.config_write(0x04, 2, (1 << 0) | (1 << 2));
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dev.poll(&mut mem, &mut sink, 2);
        }));
        assert!(err.is_err());
    }
}
