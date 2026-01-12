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
use crate::io::PortIO;

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
        match (offset, size) {
            (NABM_PO_BDBAR, 4) => self.pcm_out.bdbar,
            (NABM_PO_CIV, 1) => self.pcm_out.civ() as u32,
            (NABM_PO_LVI, 1) => self.pcm_out.lvi() as u32,
            (NABM_PO_SR, 2) => self.pcm_out.sr() as u32,
            (NABM_PO_PICB, 2) => self.pcm_out.picb() as u32,
            (NABM_PO_PIV, 1) => self.pcm_out.piv() as u32,
            (NABM_PO_CR, 1) => self.pcm_out.cr() as u32,
            (NABM_GLOB_CNT, 4) => self.glob_cnt,
            (NABM_GLOB_STA, 4) => self.glob_sta,
            (NABM_ACC_SEMA, 1) => self.acc_sema as u32,
            _ => 0,
        }
    }

    /// Write to NABM (bus master) register space.
    pub fn nabm_write(&mut self, offset: u64, size: usize, value: u32) {
        match (offset, size) {
            (NABM_PO_BDBAR, 4) => self.pcm_out.write_bdbar(value),
            (NABM_PO_LVI, 1) => self.pcm_out.write_lvi(value as u8),
            (NABM_PO_SR, 2) => self.pcm_out.write_sr(value as u16),
            (NABM_PO_CR, 1) => self.pcm_out.write_cr(value as u8),
            (NABM_GLOB_CNT, 4) => {
                self.glob_cnt = value;
                if (value & (GLOB_CNT_COLD_RESET | GLOB_CNT_WARM_RESET)) != 0 {
                    self.reset();
                    return;
                }
            }
            (NABM_ACC_SEMA, 1) => {
                let _ = value;
                // Writes are ignored; semaphore always immediately available.
                self.acc_sema = 0x01;
            }
            _ => {}
        }
        self.update_irq_level();
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
        config.set_u32(0x10, u32::from(nam_base) | 0x1);
        config.set_u32(0x14, u32::from(nabm_base) | 0x1);

        // INTA#
        config.write(0x3d, 1, 1);

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
        if port >= self.bar_nam && port < self.bar_nam.wrapping_add(NAM_SIZE) {
            Some(u64::from(port.wrapping_sub(self.bar_nam)))
        } else {
            None
        }
    }

    fn port_to_nabm_offset(&self, port: u16) -> Option<u64> {
        if port >= self.bar_nabm && port < self.bar_nabm.wrapping_add(NABM_SIZE) {
            Some(u64::from(port.wrapping_sub(self.bar_nabm)))
        } else {
            None
        }
    }
}

impl PciDevice for Ac97PciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if size == 4 {
            match offset {
                0x10 => {
                    return if self.bar0_probe {
                        !(u32::from(NAM_SIZE) - 1) | 0x1
                    } else {
                        u32::from(self.bar_nam) | 0x1
                    };
                }
                0x14 => {
                    return if self.bar1_probe {
                        !(u32::from(NABM_SIZE) - 1) | 0x1
                    } else {
                        u32::from(self.bar_nabm) | 0x1
                    };
                }
                _ => {}
            }
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if offset == 0x10 && size == 4 {
            if value == 0xffff_ffff {
                self.bar0_probe = true;
            } else {
                self.bar0_probe = false;
                self.bar_nam = (value & !0x3) as u16;
                self.config.set_u32(0x10, u32::from(self.bar_nam) | 0x1);
            }
            return;
        }
        if offset == 0x14 && size == 4 {
            if value == 0xffff_ffff {
                self.bar1_probe = true;
            } else {
                self.bar1_probe = false;
                self.bar_nabm = (value & !0x3) as u16;
                self.config.set_u32(0x14, u32::from(self.bar_nabm) | 0x1);
            }
            return;
        }

        self.config.write(offset, size, value);
    }
}

impl PortIO for Ac97PciDevice {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        }
        if let Some(off) = self.port_to_nam_offset(port) {
            return self.controller.nam_read(off, size);
        }
        if let Some(off) = self.port_to_nabm_offset(port) {
            return self.controller.nabm_read(off, size);
        }
        0
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return;
        }
        if let Some(off) = self.port_to_nam_offset(port) {
            self.controller.nam_write(off, size, val);
            return;
        }
        if let Some(off) = self.port_to_nabm_offset(port) {
            self.controller.nabm_write(off, size, val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::PortIO;

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
    fn pci_wrapper_gates_ac97_ports_on_pci_command_io_bit() {
        let mut dev = Ac97PciDevice::new(0x1000, 0x1100);

        // With COMMAND.IO clear, reads float high and writes are ignored.
        let vid1_port = 0x1000u16 + NAM_VENDOR_ID1 as u16;
        assert_eq!(dev.port_read(vid1_port, 2), 0xffff);

        let master_vol_port = 0x1000u16 + NAM_MASTER_VOL as u16;
        dev.port_write(master_vol_port, 2, 0x1234);

        // Enable IO decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 0);
        assert_eq!(dev.port_read(vid1_port, 2) as u16, 0x8384);
        assert_eq!(dev.port_read(master_vol_port, 2) as u16, 0x0000);
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
        dev.port_write(0x1100 + NABM_PO_BDBAR as u16, 4, 0x2000);
        dev.port_write(0x1100 + NABM_PO_LVI as u16, 1, 0);
        dev.port_write(0x1100 + NABM_PO_CR as u16, 1, u32::from(CR_RPBM));

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
