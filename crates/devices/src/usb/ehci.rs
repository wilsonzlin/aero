//! EHCI (USB 2.0) controller integrated into Aero's canonical PCI + MMIO device stack.
//!
//! The controller implementation lives in `aero-usb`; this module provides the glue to expose it
//! as a PCI function with an MMIO BAR.
//!
//! Design notes + emulator/runtime contracts: see `docs/usb-ehci.md`.

use crate::pci::profile::USB_EHCI_ICH9;
use crate::pci::{PciBarKind, PciConfigSpace, PciConfigSpaceState, PciDevice};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::memory::MemoryBus;
use aero_usb::ehci::EhciController;
pub use aero_usb::ehci::{regs, regs::*};
use memory::MmioHandler;

/// PCI wrapper for an emulated EHCI controller.
///
/// This device exposes an Intel ICH9-family EHCI identity (widely supported by Windows 7 inbox
/// drivers), including:
/// - class code 0x0c0320 (serial bus / USB / EHCI)
/// - BAR0 MMIO window size 0x1000
/// - interrupt pin INTA#
pub struct EhciPciDevice {
    config: PciConfigSpace,
    controller: EhciController,
}

impl EhciPciDevice {
    /// EHCI MMIO register block size (BAR0).
    pub const MMIO_BAR_SIZE: u32 = MMIO_SIZE;
    /// EHCI MMIO BAR index (BAR0).
    pub const MMIO_BAR_INDEX: u8 = 0;

    pub fn new() -> Self {
        Self::new_with_controller(EhciController::new())
    }

    pub fn new_with_controller(controller: EhciController) -> Self {
        let config = USB_EHCI_ICH9.build_config_space();
        Self { config, controller }
    }

    pub fn controller(&self) -> &EhciController {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut EhciController {
        &mut self.controller
    }

    fn mmio_decode_enabled(&self) -> bool {
        // PCI command bit 1 enables memory space decoding.
        (self.config.command() & (1 << 1)) != 0
    }

    /// Returns the current PCI BAR0 range if it is a programmed MMIO BAR.
    pub fn bar0_range(&self) -> Option<(u64, u64)> {
        let range = self.config.bar_range(Self::MMIO_BAR_INDEX)?;
        if !matches!(range.kind, PciBarKind::Mmio32 | PciBarKind::Mmio64) {
            return None;
        }
        Some((range.base, range.size))
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
            fn dma_enabled(&self) -> bool {
                matches!(self, AeroUsbMemoryBus::Dma(_))
            }

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
        // access guest memory for schedule structures.
        let dma_enabled = (self.config.command() & (1 << 2)) != 0;
        let mut adapter = if dma_enabled {
            AeroUsbMemoryBus::Dma(mem)
        } else {
            AeroUsbMemoryBus::NoDma
        };

        self.controller.tick_1ms(&mut adapter);
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
        // Preserve BAR programming but disable decoding + bus mastering.
        self.config.set_command(0);

        // Reset controller registers while keeping attached device models.
        self.controller.mmio_write(REG_USBCMD, 4, USBCMD_HCRESET);
    }
}

impl MmioHandler for EhciPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if size > 8 {
            return u64::MAX;
        }

        if !self.mmio_decode_enabled() {
            return all_ones(size);
        }

        let end = match offset.checked_add(size as u64) {
            Some(v) => v,
            None => return all_ones(size),
        };
        if end > u64::from(MMIO_SIZE) {
            return all_ones(size);
        }

        let v = if size <= 4 {
            u64::from(self.controller.mmio_read(offset, size))
        } else {
            // Conservatively handle wide reads as byte-wise accesses so we don't depend on the
            // controller's internal access-size handling.
            let mut out = 0u64;
            for i in 0..size {
                let byte = self.controller.mmio_read(offset + i as u64, 1) as u8;
                out |= u64::from(byte) << (i * 8);
            }
            out
        };

        // Mask to avoid leaking junk in upper bits for sub-8-byte reads.
        v & all_ones(size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        if !self.mmio_decode_enabled() {
            return;
        }

        let end = match offset.checked_add(size as u64) {
            Some(v) => v,
            None => return,
        };
        if end > u64::from(MMIO_SIZE) {
            return;
        }

        // Mask to enforce byte-enable semantics even for handlers that treat `value` as a full
        // 64-bit quantity.
        let masked = value & all_ones(size);

        if size <= 4 {
            self.controller
                .mmio_write(offset, size, u32::try_from(masked).unwrap_or(u32::MAX));
        } else {
            // Wide writes are not used by typical EHCI drivers; implement a conservative byte-wise
            // fallback.
            for i in 0..size {
                let byte = ((masked >> (i * 8)) & 0xff) as u32;
                self.controller.mmio_write(offset + i as u64, 1, byte);
            }
        }
    }
}

impl IoSnapshot for EhciPciDevice {
    const DEVICE_ID: [u8; 4] = *b"EHCP";
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
                "missing ehci controller state",
            ));
        };
        self.controller.load_state(buf)?;

        Ok(())
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
    use aero_platform::address_filter::AddressFilter;
    use aero_platform::chipset::ChipsetState;
    use memory::{GuestMemory, GuestMemoryError, GuestMemoryResult};
    use std::sync::{Arc, Mutex};

    #[test]
    fn config_matches_profile() {
        let dev = EhciPciDevice::default();
        let id = dev.config.vendor_device_id();
        assert_eq!(
            id.vendor_id, PCI_VENDOR_ID_INTEL,
            "EHCI should use Intel vendor ID"
        );
        assert_eq!(
            id.device_id, PCI_DEVICE_ID_INTEL_ICH9_EHCI,
            "EHCI should use ICH9 EHCI device ID"
        );

        assert_eq!(
            dev.config.bar_definition(EhciPciDevice::MMIO_BAR_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: MMIO_SIZE,
                prefetchable: false
            })
        );
    }

    #[test]
    fn pci_command_mem_enable_gates_mmio_decode() {
        let mut dev = EhciPciDevice::default();

        // Default command register has MEM decoding disabled, so reads should float high.
        assert_eq!(MmioHandler::read(&mut dev, 0, 4), 0xFFFF_FFFF);

        // Writes should be ignored while disabled.
        MmioHandler::write(&mut dev, REG_USBCMD, 4, 0x1122_3344);

        // Enable MEM decoding and verify BAR0 reads now dispatch to the controller.
        dev.config.set_command(0x0002);
        let cap = MmioHandler::read(&mut dev, 0, 4) as u32;
        assert_ne!(cap, 0xFFFF_FFFF);

        // The earlier write should have been ignored.
        assert_eq!(MmioHandler::read(&mut dev, REG_USBCMD, 4) as u32, 0);

        // With decoding enabled, writes should reach the controller.
        MmioHandler::write(&mut dev, REG_USBCMD, 4, 0xAABB_CCDD);
        let expected = 0xAABB_CCDD_u32 & USBCMD_WRITE_MASK;
        assert_eq!(MmioHandler::read(&mut dev, REG_USBCMD, 4) as u32, expected);
    }

    #[test]
    fn pci_command_bus_master_enable_gates_dma() {
        #[derive(Debug)]
        struct RamState {
            mem: Vec<u8>,
            reads: usize,
            writes: usize,
        }

        #[derive(Clone)]
        struct RecordingRam {
            state: Arc<Mutex<RamState>>,
        }

        impl RecordingRam {
            fn new(size: usize) -> (Self, Arc<Mutex<RamState>>) {
                let state = Arc::new(Mutex::new(RamState {
                    mem: vec![0; size],
                    reads: 0,
                    writes: 0,
                }));
                (
                    Self {
                        state: state.clone(),
                    },
                    state,
                )
            }
        }

        impl GuestMemory for RecordingRam {
            fn size(&self) -> u64 {
                self.state.lock().unwrap().mem.len() as u64
            }

            fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
                let mut st = self.state.lock().unwrap();
                let size = st.mem.len() as u64;
                let len = dst.len();
                let end = paddr
                    .checked_add(len as u64)
                    .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
                if end > size {
                    return Err(GuestMemoryError::OutOfRange { paddr, len, size });
                }
                let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
                    paddr,
                    len,
                    size,
                })?;
                let end = start.checked_add(len).ok_or(GuestMemoryError::OutOfRange {
                    paddr,
                    len,
                    size,
                })?;

                st.reads += 1;
                dst.copy_from_slice(&st.mem[start..end]);
                Ok(())
            }

            fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
                let mut st = self.state.lock().unwrap();
                let size = st.mem.len() as u64;
                let len = src.len();
                let end = paddr
                    .checked_add(len as u64)
                    .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
                if end > size {
                    return Err(GuestMemoryError::OutOfRange { paddr, len, size });
                }
                let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
                    paddr,
                    len,
                    size,
                })?;
                let end = start.checked_add(len).ok_or(GuestMemoryError::OutOfRange {
                    paddr,
                    len,
                    size,
                })?;

                st.writes += 1;
                st.mem[start..end].copy_from_slice(src);
                Ok(())
            }
        }

        let (ram, ram_state) = RecordingRam::new(0x10000);
        let chipset = ChipsetState::new(true);
        let filter = AddressFilter::new(chipset.a20());
        let mut bus = MemoryBus::with_ram(filter, Box::new(ram));

        let mut dev = EhciPciDevice::default();

        // Program the controller to attempt a periodic schedule DMA.
        dev.config.set_command(0x0002); // MEM enable, bus master disabled.
        MmioHandler::write(&mut dev, REG_PERIODICLISTBASE, 4, 0x2000);
        MmioHandler::write(&mut dev, REG_USBCMD, 4, u64::from(USBCMD_RS | USBCMD_PSE));

        // With bus mastering disabled, the wrapper must prevent any physical memory reads/writes.
        dev.tick_1ms(&mut bus);
        {
            let st = ram_state.lock().unwrap();
            assert_eq!(st.reads, 0);
            assert_eq!(st.writes, 0);
        }

        // Enable bus mastering and ensure the controller can now touch guest RAM.
        dev.config.set_command(0x0002 | 0x0004);
        dev.tick_1ms(&mut bus);
        {
            let st = ram_state.lock().unwrap();
            assert!(st.reads > 0, "expected at least one DMA read");
        }
    }

    #[test]
    fn pci_command_intx_disable_bit_masks_irq_level() {
        let mut dev = EhciPciDevice::default();

        // Enable MMIO decoding so we can program USBINTR.
        dev.config.set_command(0x0002);
        MmioHandler::write(&mut dev, REG_USBINTR, 4, u64::from(USBINTR_USBINT));
        dev.controller_mut().set_usbsts_bits(USBSTS_USBINT);

        assert!(dev.controller.irq_level());
        assert!(dev.irq_level());

        // Disable legacy INTx delivery via PCI command bit 10.
        dev.config.set_command(0x0002 | (1 << 10));
        assert!(dev.controller.irq_level());
        assert!(!dev.irq_level());
    }
}
