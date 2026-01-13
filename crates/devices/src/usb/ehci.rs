//! EHCI (USB 2.0) controller exposed via Aero's PCI + MMIO device stack.
//!
//! This is currently a minimal EHCI register model intended for platform wiring and smoke tests.
//! Full USB2 transaction support will be implemented in `aero-usb` and integrated here.

use crate::pci::profile::USB_EHCI_ICH9;
use crate::pci::{PciConfigSpace, PciDevice};
use aero_platform::memory::MemoryBus;
use memory::MmioHandler;

const PCI_COMMAND_MEM_ENABLE: u16 = 1 << 1;

// Capability registers (offset 0x00..0x1f). Operational registers begin at CAPLENGTH.
const CAPLENGTH: u8 = 0x20;
const HCIVERSION: u16 = 0x0100;

const REG_CAPLENGTH_HCIVERSION: u64 = 0x00;
const REG_HCSPARAMS: u64 = 0x04;
const REG_HCCPARAMS: u64 = 0x08;
const REG_HCSP_PORTROUTE: u64 = 0x0c;

// Operational registers (offset 0x20..).
const REG_USBCMD: u64 = 0x20;
const REG_USBSTS: u64 = 0x24;
const REG_USBINTR: u64 = 0x28;
const REG_FRINDEX: u64 = 0x2c;
const REG_PERIODICLISTBASE: u64 = 0x34;
const REG_ASYNCLISTADDR: u64 = 0x38;
const REG_CONFIGFLAG: u64 = 0x40;
const REG_PORTSC_BASE: u64 = 0x44;

const NUM_PORTS: usize = 4;

/// PCI wrapper for an emulated EHCI controller.
///
/// The canonical PCI identity is sourced from [`USB_EHCI_ICH9`] to match Windows inbox drivers.
pub struct EhciPciDevice {
    config: PciConfigSpace,

    // Minimal operational register model.
    usbcmd: u32,
    usbsts: u32,
    usbintr: u32,
    frindex: u32,
    periodiclistbase: u32,
    asynclistaddr: u32,
    configflag: u32,
    portsc: [u32; NUM_PORTS],
}

impl EhciPciDevice {
    /// EHCI register block size (BAR0 MMIO).
    pub const MMIO_BAR_SIZE: u32 = 0x1000;
    /// EHCI MMIO BAR index (BAR0).
    pub const MMIO_BAR_INDEX: u8 = 0;

    pub fn new() -> Self {
        Self {
            config: USB_EHCI_ICH9.build_config_space(),
            usbcmd: 0,
            usbsts: 0,
            usbintr: 0,
            frindex: 0,
            periodiclistbase: 0,
            asynclistaddr: 0,
            configflag: 0,
            portsc: [0; NUM_PORTS],
        }
    }

    /// Returns the current level of the device's legacy INTx line.
    pub fn irq_level(&self) -> bool {
        // PCI command bit 10 disables legacy INTx assertion.
        let intx_disabled = (self.config.command() & (1 << 10)) != 0;
        if intx_disabled {
            return false;
        }

        // Minimal IRQ model: assert if any enabled interrupt status bit is set.
        (self.usbsts & self.usbintr & 0x3f) != 0
    }

    /// Advance the controller by 1ms.
    ///
    /// This is currently a timing placeholder so the platform can keep EHCI state deterministic.
    pub fn tick_1ms(&mut self, _mem: &mut MemoryBus) {
        // FRINDEX increments once per micro-frame (125us). Advance by 8 micro-frames per 1ms.
        //
        // Keep it 14-bit as defined by the spec.
        self.frindex = (self.frindex.wrapping_add(8)) & 0x3fff;
    }

    fn read_reg_u32(&self, word_off: u64) -> u32 {
        match word_off {
            REG_CAPLENGTH_HCIVERSION => {
                // [7:0] CAPLENGTH, [15:8] reserved, [31:16] HCIVERSION.
                (u32::from(HCIVERSION) << 16) | u32::from(CAPLENGTH)
            }
            REG_HCSPARAMS => {
                // [3:0] N_PORTS.
                (NUM_PORTS as u32) & 0xf
            }
            REG_HCCPARAMS => 0,
            REG_HCSP_PORTROUTE => 0,
            REG_USBCMD => self.usbcmd,
            REG_USBSTS => self.usbsts,
            REG_USBINTR => self.usbintr,
            REG_FRINDEX => self.frindex,
            REG_PERIODICLISTBASE => self.periodiclistbase,
            REG_ASYNCLISTADDR => self.asynclistaddr,
            REG_CONFIGFLAG => self.configflag,
            off if off >= REG_PORTSC_BASE
                && off < REG_PORTSC_BASE + (NUM_PORTS as u64) * 4
                && (off - REG_PORTSC_BASE) % 4 == 0 =>
            {
                let idx = ((off - REG_PORTSC_BASE) / 4) as usize;
                self.portsc[idx]
            }
            _ => 0,
        }
    }

    fn write_reg_u32(&mut self, word_off: u64, value: u32, mask: u32) {
        // For partial writes, merge per-byte using the provided mask.
        let merge = |dst: &mut u32| {
            *dst = (*dst & !mask) | (value & mask);
        };

        match word_off {
            // Capability registers are read-only.
            REG_CAPLENGTH_HCIVERSION | REG_HCSPARAMS | REG_HCCPARAMS | REG_HCSP_PORTROUTE => {}
            REG_USBCMD => merge(&mut self.usbcmd),
            REG_USBINTR => merge(&mut self.usbintr),
            REG_FRINDEX => merge(&mut self.frindex),
            REG_PERIODICLISTBASE => merge(&mut self.periodiclistbase),
            REG_ASYNCLISTADDR => merge(&mut self.asynclistaddr),
            REG_CONFIGFLAG => merge(&mut self.configflag),
            REG_USBSTS => {
                // RW1C semantics for interrupt/status bits (minimal).
                let bits = value & mask;
                self.usbsts &= !bits;
            }
            off if off >= REG_PORTSC_BASE
                && off < REG_PORTSC_BASE + (NUM_PORTS as u64) * 4
                && (off - REG_PORTSC_BASE) % 4 == 0 =>
            {
                let idx = ((off - REG_PORTSC_BASE) / 4) as usize;
                let cur = self.portsc[idx];
                self.portsc[idx] = (cur & !mask) | (value & mask);
            }
            _ => {}
        }
    }
}

impl Default for EhciPciDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl PciDevice for EhciPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        // Preserve BAR programming but disable decoding.
        self.config.set_command(0);

        self.usbcmd = 0;
        self.usbsts = 0;
        self.usbintr = 0;
        self.frindex = 0;
        self.periodiclistbase = 0;
        self.asynclistaddr = 0;
        self.configflag = 0;
        self.portsc = [0; NUM_PORTS];
    }
}

impl MmioHandler for EhciPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.clamp(1, 8);

        if (self.config.command() & PCI_COMMAND_MEM_ENABLE) == 0 {
            return all_ones(size);
        }

        let mut out = 0u64;
        for i in 0..size {
            let byte_off = match offset.checked_add(i as u64) {
                Some(v) => v,
                None => break,
            };
            if byte_off >= u64::from(Self::MMIO_BAR_SIZE) {
                out |= 0xffu64 << (i * 8);
                continue;
            }
            let word_off = byte_off & !3;
            let shift = ((byte_off & 3) * 8) as u32;
            let word = self.read_reg_u32(word_off);
            out |= u64::from((word >> shift) as u8) << (i * 8);
        }

        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.clamp(1, 8);

        if (self.config.command() & PCI_COMMAND_MEM_ENABLE) == 0 {
            return;
        }

        let mut idx = 0usize;
        while idx < size {
            let byte_off = match offset.checked_add(idx as u64) {
                Some(v) => v,
                None => break,
            };
            if byte_off >= u64::from(Self::MMIO_BAR_SIZE) {
                idx += 1;
                continue;
            }

            let word_off = byte_off & !3;
            let shift = ((byte_off & 3) * 8) as u32;
            let mask = 0xffu32 << shift;
            let byte = ((value >> (idx * 8)) & 0xff) as u32;
            let v = byte << shift;
            self.write_reg_u32(word_off, v, mask);
            idx += 1;
        }
    }
}

fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::profile::{PCI_DEVICE_ID_INTEL_ICH9_EHCI, PCI_VENDOR_ID_INTEL};
    use crate::pci::PciBarDefinition;

    #[test]
    fn config_matches_profile() {
        let dev = EhciPciDevice::default();
        let id = dev.config.vendor_device_id();
        assert_eq!(id.vendor_id, PCI_VENDOR_ID_INTEL);
        assert_eq!(id.device_id, PCI_DEVICE_ID_INTEL_ICH9_EHCI);

        assert_eq!(
            dev.config.bar_definition(EhciPciDevice::MMIO_BAR_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: EhciPciDevice::MMIO_BAR_SIZE,
                prefetchable: false
            })
        );
    }
}
