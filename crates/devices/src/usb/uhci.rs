//! UHCI (USB 1.1) controller integrated into Aero's canonical PCI + port-I/O device stack.
//!
//! The controller implementation lives in `aero-usb`; this module provides the glue to expose it
//! as a PCI function with an I/O BAR.

use crate::pci::profile::USB_UHCI_PIIX3;
use crate::pci::{PciBarKind, PciConfigSpace, PciConfigSpaceState, PciDevice};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::io::{IoPortBus, PortIoDevice};
use aero_platform::memory::MemoryBus;
use aero_usb::uhci::UhciController;
pub use aero_usb::uhci::{regs, regs::*};
use std::cell::RefCell;
use std::rc::Rc;

/// PCI wrapper for an emulated UHCI controller.
///
/// This device exposes an Intel PIIX3-style UHCI identity (widely supported by Windows 7 inbox
/// drivers), including:
/// - class code 0x0c0300 (serial bus / USB / UHCI)
/// - BAR4 I/O window size 0x20
/// - interrupt pin INTA# and a typical interrupt line (11 when placed at 00:01.2)
pub struct UhciPciDevice {
    config: PciConfigSpace,
    controller: UhciController,
}

impl UhciPciDevice {
    /// UHCI register block size (BAR4 I/O).
    pub const IO_BAR_SIZE: u16 = 0x20;
    /// UHCI I/O BAR index (BAR4).
    pub const IO_BAR_INDEX: u8 = 4;

    pub fn new(controller: UhciController) -> Self {
        let config = USB_UHCI_PIIX3.build_config_space();
        Self { config, controller }
    }

    pub fn controller(&self) -> &UhciController {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut UhciController {
        &mut self.controller
    }

    pub fn irq_level(&self) -> bool {
        // PCI command bit 10 disables legacy INTx assertion.
        let intx_disabled = (self.config.command() & (1 << 10)) != 0;
        if intx_disabled {
            return false;
        }
        self.controller.irq_level()
    }

    /// Advance the controller by 1ms using the platform's canonical physical memory bus.
    pub fn tick_1ms(&mut self, mem: &mut MemoryBus) {
        enum AeroUsbMemoryBus<'a> {
            Dma(&'a mut MemoryBus),
            NoDma,
        }

        impl aero_usb::MemoryBus for AeroUsbMemoryBus<'_> {
            fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
                match self {
                    AeroUsbMemoryBus::Dma(inner) => inner.read_physical(paddr, buf),
                    AeroUsbMemoryBus::NoDma => buf.fill(0xFF),
                }
            }

            fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
                match self {
                    AeroUsbMemoryBus::Dma(inner) => inner.write_physical(paddr, buf),
                    AeroUsbMemoryBus::NoDma => {}
                }
            }
        }

        // Gate DMA on PCI command Bus Master Enable (bit 2). When bus mastering is disabled the
        // controller still advances its internal frame counter and root hub state, but it must not
        // access guest memory for the schedule/frame list.
        let dma_enabled = (self.config.command() & (1 << 2)) != 0;
        let mut adapter = if dma_enabled {
            AeroUsbMemoryBus::Dma(mem)
        } else {
            AeroUsbMemoryBus::NoDma
        };

        self.controller.tick_1ms(&mut adapter);
    }

    fn io_bar_range(&self) -> Option<(u16, u16)> {
        let range = self.config.bar_range(Self::IO_BAR_INDEX)?;
        if range.kind != PciBarKind::Io {
            return None;
        }
        let base = u16::try_from(range.base).ok()?;
        let size = u16::try_from(range.size).ok()?;
        Some((base, size))
    }

    fn io_decode_enabled(&self) -> bool {
        (self.config.command() & 0x1) != 0
    }

    pub fn port_read(&mut self, port: u16, size: u8) -> u32 {
        let size_usize = match size {
            1 | 2 | 4 => usize::from(size),
            _ => return all_ones(size),
        };

        if !self.io_decode_enabled() {
            return all_ones(size);
        }

        let Some((base, len)) = self.io_bar_range() else {
            return all_ones(size);
        };
        let Some(end) = base.checked_add(len) else {
            return all_ones(size);
        };
        if port < base || port >= end {
            return all_ones(size);
        }

        let offset = port - base;
        self.controller.io_read(offset, size_usize)
    }

    pub fn port_write(&mut self, port: u16, size: u8, value: u32) {
        let size_usize = match size {
            1 | 2 | 4 => usize::from(size),
            _ => return,
        };

        if !self.io_decode_enabled() {
            return;
        }

        let Some((base, len)) = self.io_bar_range() else {
            return;
        };
        let Some(end) = base.checked_add(len) else {
            return;
        };
        if port < base || port >= end {
            return;
        }

        let offset = port - base;
        self.controller.io_write(offset, size_usize, value);
    }
}

impl Default for UhciPciDevice {
    fn default() -> Self {
        Self::new(UhciController::new())
    }
}

impl PciDevice for UhciPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        // Preserve BAR programming but disable decoding.
        self.config.set_command(0);

        // Reset controller registers while keeping attached device models.
        self.controller
            .io_write(REG_USBCMD, 2, u32::from(USBCMD_HCRESET));
    }
}

impl IoSnapshot for UhciPciDevice {
    const DEVICE_ID: [u8; 4] = *b"UHCP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let pci = self.config.snapshot_state();
        let mut pci_enc = Encoder::new().bytes(&pci.bytes);
        for i in 0..6 {
            pci_enc = pci_enc.u64(pci.bar_base[i]).bool(pci.bar_probe[i]);
        }
        w.field_bytes(TAG_PCI, pci_enc.finish());

        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_PCI) {
            let mut d = Decoder::new(buf);
            let mut config_bytes = [0u8; crate::pci::capabilities::PCI_CONFIG_SPACE_SIZE];
            let len = config_bytes.len();
            config_bytes.copy_from_slice(d.bytes(len)?);

            let mut bar_base = [0u64; 6];
            let mut bar_probe = [false; 6];
            for i in 0..6 {
                bar_base[i] = d.u64()?;
                bar_probe[i] = d.bool()?;
            }
            d.finish()?;

            self.config.restore_state(&PciConfigSpaceState {
                bytes: config_bytes,
                bar_base,
                bar_probe,
            });
        }

        let Some(buf) = r.bytes(TAG_CONTROLLER) else {
            return Err(SnapshotError::InvalidFieldEncoding(
                "missing uhci controller state",
            ));
        };
        self.controller.load_state(buf)?;

        Ok(())
    }
}

fn all_ones(size: u8) -> u32 {
    match size {
        0 => 0,
        1 => 0xff,
        2 => 0xffff,
        4 => 0xffff_ffff,
        _ => 0xffff_ffff,
    }
}

pub type SharedUhciPciDevice = Rc<RefCell<UhciPciDevice>>;

/// I/O-port view of a shared [`UhciPciDevice`].
///
/// `IoPortBus` maps one port to one device instance; UHCI exposes a 0x20-byte I/O window, so the
/// common pattern is to share the controller behind `Rc<RefCell<_>>` and register one
/// `UhciPciPort` per port.
pub struct UhciPciPort {
    dev: SharedUhciPciDevice,
    port: u16,
}

impl UhciPciPort {
    pub fn new(dev: SharedUhciPciDevice, port: u16) -> Self {
        Self { dev, port }
    }
}

impl PortIoDevice for UhciPciPort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.dev.borrow_mut().port_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.dev.borrow_mut().port_write(port, size, value);
    }
}

/// Convenience helper to register UHCI's I/O BAR ports on an [`IoPortBus`].
///
/// This expects BAR4 to be programmed with a non-zero base address.
pub fn register_uhci_io_ports(bus: &mut IoPortBus, dev: SharedUhciPciDevice) {
    let (base, len) = dev
        .borrow()
        .io_bar_range()
        .expect("UHCI BAR4 must be programmed before registering I/O ports");
    assert_eq!(
        len,
        UhciPciDevice::IO_BAR_SIZE,
        "unexpected UHCI BAR size (expected {:x}, got {:x})",
        UhciPciDevice::IO_BAR_SIZE,
        len
    );

    let end = base.checked_add(len).expect("UHCI BAR port range overflow");
    for port in base..end {
        bus.register(port, Box::new(UhciPciPort::new(dev.clone(), port)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::profile::{PCI_DEVICE_ID_INTEL_PIIX3_UHCI, PCI_VENDOR_ID_INTEL};
    use crate::pci::PciBarDefinition;

    #[test]
    fn config_matches_profile() {
        let dev = UhciPciDevice::default();
        let id = dev.config.vendor_device_id();
        assert_eq!(
            id.vendor_id, PCI_VENDOR_ID_INTEL,
            "UHCI should use Intel vendor ID"
        );
        assert_eq!(
            id.device_id, PCI_DEVICE_ID_INTEL_PIIX3_UHCI,
            "UHCI should use PIIX3 UHCI device ID"
        );

        assert_eq!(
            dev.config.bar_definition(UhciPciDevice::IO_BAR_INDEX),
            Some(PciBarDefinition::Io {
                size: u32::from(UhciPciDevice::IO_BAR_SIZE)
            })
        );
    }
}
