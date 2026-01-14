//! xHCI (USB 3.x) controller exposed as a PCI function.
//!
//! This module provides the canonical "PCI glue" needed to instantiate the shared
//! [`aero_usb::xhci::XhciController`] model on a PCI bus:
//! - PCI config space identity + BAR0 definition
//! - MMIO read/write dispatch into the controller register model
//! - PCI `COMMAND` gating:
//!   - MMIO decode is gated on `COMMAND.MEM` (bit 1)
//!   - DMA is gated on `COMMAND.BME` (bit 2)
//!   - legacy INTx signalling is gated on `COMMAND.INTX_DISABLE` (bit 10)
//! - Optional MSI delivery via the device's MSI capability

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::interrupts::msi::MsiTrigger;
use aero_platform::memory::MemoryBus;
use memory::MmioHandler;

use crate::irq::IrqLine;
use crate::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use crate::pci::{profile, MsiCapability, PciConfigSpace, PciConfigSpaceState, PciDevice};

pub use aero_usb::xhci::{regs, XhciController};

/// Minimal IRQ line implementation that can be shared with an underlying controller.
#[derive(Clone, Default)]
struct AtomicIrqLine {
    level: Arc<AtomicBool>,
}

impl AtomicIrqLine {
    fn level(&self) -> bool {
        self.level.load(Ordering::SeqCst)
    }
}

impl IrqLine for AtomicIrqLine {
    fn set_level(&self, level: bool) {
        self.level.store(level, Ordering::SeqCst);
    }
}

/// PCI wrapper for a (native) xHCI controller.
///
/// The wrapper maintains:
/// - PCI configuration space (including MSI capability state)
/// - An internal interrupt condition that can be surfaced via:
///   - legacy INTx (`irq_level()`), or
///   - MSI (`service_interrupts()` when MSI is enabled and a target is configured).
pub struct XhciPciDevice {
    config: PciConfigSpace,
    controller: XhciController,
    irq: AtomicIrqLine,
    dma_mem: Option<Rc<RefCell<dyn memory::MemoryBus>>>,
    msi_target: Option<Box<dyn MsiTrigger>>,
    last_irq_level: bool,
}

impl XhciPciDevice {
    /// xHCI MMIO BAR size (BAR0).
    ///
    /// Keep in sync with [`crate::pci::profile::XHCI_MMIO_BAR_SIZE_U32`].
    pub const MMIO_BAR_SIZE: u32 = profile::XHCI_MMIO_BAR_SIZE_U32;
    /// xHCI MMIO BAR index (BAR0).
    pub const MMIO_BAR_INDEX: u8 = profile::XHCI_MMIO_BAR_INDEX;

    /// Create a new xHCI PCI device wrapper with the canonical controller model.
    pub fn new() -> Self {
        Self::new_with_controller(XhciController::new())
    }

    /// Create a new xHCI PCI device wrapper around the provided controller instance.
    pub fn new_with_controller(controller: XhciController) -> Self {
        let irq = AtomicIrqLine::default();

        // Start from the canonical QEMU-style xHCI PCI profile so BAR definitions, class code, and
        // capabilities stay consistent across runtimes.
        //
        // Keep this in sync with:
        // - `crates/devices/src/pci/profile.rs` (`USB_XHCI_QEMU`),
        // - `web/src/io/devices/xhci.ts` (web runtime wrapper), and
        // - `docs/usb-xhci.md` (guest-facing contract).
        let mut config = profile::USB_XHCI_QEMU.build_config_space();

        // Backwards compatibility: older profiles may omit MSI. Ensure we always expose at least a
        // single-vector MSI capability so platforms/tests can route interrupts without extra
        // vendor-specific plumbing.
        if config.capability::<MsiCapability>().is_none() {
            config.add_capability(Box::new(MsiCapability::new()));
        }

        Self {
            config,
            controller,
            irq,
            dma_mem: None,
            msi_target: None,
            last_irq_level: false,
        }
    }

    /// Configure the memory bus used for controller DMA.
    ///
    /// This is optional; when unset, DMA accesses are treated as disabled (reads return `0xFF`,
    /// writes are ignored) even if PCI bus mastering is enabled.
    pub fn set_dma_memory_bus(&mut self, bus: Option<Rc<RefCell<dyn memory::MemoryBus>>>) {
        self.dma_mem = bus;
    }

    pub fn controller(&self) -> &XhciController {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut XhciController {
        &mut self.controller
    }

    /// Configure the target used for MSI interrupt delivery.
    ///
    /// Platform integrations should provide a sink that injects the programmed MSI message into the
    /// guest (e.g. `PlatformInterrupts` in APIC mode).
    ///
    /// If no target is configured, the device falls back to legacy INTx signalling even when the
    /// guest enables MSI in PCI config space. This preserves compatibility with platforms that do
    /// not support MSI delivery.
    pub fn set_msi_target(&mut self, target: Option<Box<dyn MsiTrigger>>) {
        self.msi_target = target;
    }

    fn msi_enabled(&self) -> bool {
        self.config
            .capability::<MsiCapability>()
            .is_some_and(|cap| cap.enabled())
    }

    fn msi_active(&self) -> bool {
        self.msi_target.is_some() && self.msi_enabled()
    }

    fn service_interrupts(&mut self) {
        let level = self.irq.level() || self.controller.irq_level();

        // MSI delivery is edge-triggered. Fire on a rising edge of the interrupt condition.
        if level && !self.last_irq_level {
            if let (Some(target), Some(msi)) = (
                self.msi_target.as_mut(),
                self.config.capability_mut::<MsiCapability>(),
            ) {
                // Ignore the return value: if the guest masked the vector, the capability will set
                // its pending bit and we should not fall back to INTx while MSI is enabled.
                let _ = msi.trigger(&mut **target);
            }
        }

        self.last_irq_level = level;
    }

    /// Returns the current level of the device's legacy INTx line.
    ///
    /// This is gated by PCI `COMMAND.INTX_DISABLE` (bit 10) and is suppressed while MSI is active
    /// so interrupts are not delivered twice.
    pub fn irq_level(&self) -> bool {
        if self.msi_active() {
            return false;
        }

        // PCI command bit 10 disables legacy INTx assertion.
        if (self.config.command() & (1 << 10)) != 0 {
            return false;
        }

        self.irq.level() || self.controller.irq_level()
    }

    /// Raises the internal interrupt condition, and delivers an MSI message if configured.
    ///
    /// Platform integrations that model the full xHCI register set should call this when the xHCI
    /// interrupt condition becomes asserted (e.g. upon adding an event TRB).
    pub fn raise_event_interrupt(&mut self) {
        self.irq.set_level(true);
        self.service_interrupts();
    }

    /// Clears the internal interrupt condition.
    pub fn clear_event_interrupt(&mut self) {
        self.irq.set_level(false);
        self.service_interrupts();
    }

    /// Test/harness hook: inject a Port Status Change Event on interrupter 0.
    ///
    /// This exists so platform integration tests can validate PCI INTx wiring without booting a
    /// guest OS. Callers are expected to have configured the event ring (ERST*) and enabled
    /// IMAN.IE.
    pub fn trigger_port_status_change_event(&mut self, mem: &mut MemoryBus) {
        use aero_usb::xhci::trb::{Trb, TrbType};

        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::PortStatusChangeEvent);
        self.controller.post_event(trb);

        // This helper is test-only; it always uses the provided platform memory bus, regardless of
        // PCI Bus Master Enable gating.
        let mut adapter = AeroUsbMemoryBus::Dma(mem);
        self.controller.service_event_ring(&mut adapter);
        self.service_interrupts();
    }

    /// Advance the device by 1ms.
    pub fn tick_1ms(&mut self, mem: &mut MemoryBus) {
        enum TickMemoryBus<'a> {
            Dma(&'a mut MemoryBus),
            NoDma,
        }

        impl aero_usb::MemoryBus for TickMemoryBus<'_> {
            fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
                match self {
                    TickMemoryBus::Dma(inner) => inner.read_physical(paddr, buf),
                    TickMemoryBus::NoDma => buf.fill(0xFF),
                }
            }

            fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
                match self {
                    TickMemoryBus::Dma(inner) => inner.write_physical(paddr, buf),
                    TickMemoryBus::NoDma => {
                        let _ = (paddr, buf);
                    }
                }
            }
        }

        self.controller.tick_1ms();

        // Deliver pending events into the guest-configured event ring. This requires DMA, so gate it
        // on PCI COMMAND.BME (bit 2) like other USB controllers.
        let dma_enabled = (self.config.command() & (1 << 2)) != 0;
        let mut adapter = if dma_enabled {
            TickMemoryBus::Dma(mem)
        } else {
            TickMemoryBus::NoDma
        };
        self.controller.service_event_ring(&mut adapter);

        self.service_interrupts();
    }
}

impl Default for XhciPciDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl PciDevice for XhciPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        // Preserve BAR programming but disable decoding.
        self.config.set_command(0);
        self.config.disable_msi_msix();

        self.controller = XhciController::new();
        self.irq.set_level(false);
        self.last_irq_level = false;
    }
}

enum AeroUsbMemoryBus<'a> {
    Dma(&'a mut dyn memory::MemoryBus),
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

impl MmioHandler for XhciPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if size > 8 {
            return u64::MAX;
        }

        // Gate MMIO decoding on PCI command Memory Space Enable (bit 1).
        if (self.config.command() & (1 << 1)) == 0 {
            return all_ones(size);
        }

        // Treat out-of-range BAR offsets as unmapped/open bus. This mirrors what the PCI BAR MMIO
        // router enforces, but keeping the check here makes direct unit tests deterministic.
        let end = offset.saturating_add(size as u64);
        if end > u64::from(Self::MMIO_BAR_SIZE) {
            return all_ones(size);
        }

        let dma_enabled = (self.config.command() & (1 << 2)) != 0;

        if dma_enabled {
            if let Some(mem) = self.dma_mem.as_ref() {
                let mut mem_ref = mem.borrow_mut();
                let mut adapter = AeroUsbMemoryBus::Dma(&mut *mem_ref);
                return xhci_mmio_read(&mut self.controller, &mut adapter, offset, size);
            }
        }

        let mut adapter = AeroUsbMemoryBus::NoDma;
        xhci_mmio_read(&mut self.controller, &mut adapter, offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        // Gate MMIO decoding on PCI command Memory Space Enable (bit 1).
        if (self.config.command() & (1 << 1)) == 0 {
            return;
        }

        let end = offset.saturating_add(size as u64);
        if end > u64::from(Self::MMIO_BAR_SIZE) {
            return;
        }

        let dma_enabled = (self.config.command() & (1 << 2)) != 0;
        let masked = value & all_ones(size);

        if dma_enabled {
            if let Some(mem) = self.dma_mem.as_ref() {
                {
                    let mut mem_ref = mem.borrow_mut();
                    let mut adapter = AeroUsbMemoryBus::Dma(&mut *mem_ref);
                    xhci_mmio_write(&mut self.controller, &mut adapter, offset, size, masked);
                }
                self.service_interrupts();
                return;
            }
        }

        let mut adapter = AeroUsbMemoryBus::NoDma;
        xhci_mmio_write(&mut self.controller, &mut adapter, offset, size, masked);
        self.service_interrupts();
    }
}

impl IoSnapshot for XhciPciDevice {
    const DEVICE_ID: [u8; 4] = *b"XHCP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_IRQ: u16 = 2;
        const TAG_LAST_IRQ: u16 = 3;
        const TAG_CONTROLLER: u16 = 4;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let pci = self.config.snapshot_state();
        let mut pci_enc = Encoder::new().bytes(&pci.bytes);
        for i in 0..6 {
            pci_enc = pci_enc.u64(pci.bar_base[i]).bool(pci.bar_probe[i]);
        }
        w.field_bytes(TAG_PCI, pci_enc.finish());

        w.field_bytes(TAG_IRQ, Encoder::new().bool(self.irq.level()).finish());
        w.field_bytes(TAG_LAST_IRQ, Encoder::new().bool(self.last_irq_level).finish());
        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_IRQ: u16 = 2;
        const TAG_LAST_IRQ: u16 = 3;
        const TAG_CONTROLLER: u16 = 4;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.controller = XhciController::new();
        self.irq.set_level(false);
        self.last_irq_level = false;

        if let Some(buf) = r.bytes(TAG_PCI) {
            let mut d = Decoder::new(buf);
            let mut config_bytes = [0u8; PCI_CONFIG_SPACE_SIZE];
            config_bytes.copy_from_slice(d.bytes(PCI_CONFIG_SPACE_SIZE)?);

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

        if let Some(buf) = r.bytes(TAG_IRQ) {
            let mut d = Decoder::new(buf);
            self.irq.set_level(d.bool()?);
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_CONTROLLER) {
            // Older snapshots may omit the controller field; the default controller created above
            // is a valid deterministic baseline in that case.
            self.controller.load_state(buf)?;
        }

        if let Some(buf) = r.bytes(TAG_LAST_IRQ) {
            let mut d = Decoder::new(buf);
            self.last_irq_level = d.bool()?;
            d.finish()?;
        } else {
            // Older snapshots default to the restored interrupt condition to avoid spuriously
            // generating an MSI edge immediately after restore.
            self.last_irq_level = self.controller.irq_level() || self.irq.level();
        }

        Ok(())
    }
}

fn xhci_mmio_read(
    controller: &mut XhciController,
    bus: &mut dyn aero_usb::MemoryBus,
    offset: u64,
    size: usize,
) -> u64 {
    // `XhciController` models 1/2/4-byte reads. Synthesize larger reads by composing byte-sized
    // accesses to preserve little-endian semantics.
    let mut out = 0u64;
    for i in 0..size {
        let b = controller.mmio_read(bus, offset + i as u64, 1);
        out |= (b as u64) << (i * 8);
    }
    out & all_ones(size)
}

fn xhci_mmio_write(
    controller: &mut XhciController,
    bus: &mut dyn aero_usb::MemoryBus,
    offset: u64,
    size: usize,
    value: u64,
) {
    // Prefer natural 4-byte writes when possible to avoid spurious side effects from decomposing
    // wide writes into byte writes (e.g. RUN bit edges).
    match size {
        1 | 2 | 4 => controller.mmio_write(bus, offset, size, value as u32),
        8 if (offset & 3) == 0 => {
            controller.mmio_write(bus, offset, 4, value as u32);
            controller.mmio_write(bus, offset + 4, 4, (value >> 32) as u32);
        }
        _ => {
            for i in 0..size {
                controller.mmio_write(
                    bus,
                    offset + i as u64,
                    1,
                    ((value >> (i * 8)) & 0xff) as u32,
                );
            }
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
    use super::{regs, XhciPciDevice};
    use aero_io_snapshot::io::state::IoSnapshot;
    use crate::pci::config::PciClassCode;
    use crate::pci::msi::PCI_CAP_ID_MSI;
    use crate::pci::{profile, PciBarDefinition, PciDevice};
    use memory::MmioHandler;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn exposes_msi_capability() {
        let mut dev = XhciPciDevice::default();
        assert!(
            dev.config_mut().find_capability(PCI_CAP_ID_MSI).is_some(),
            "xHCI device should expose an MSI capability"
        );
    }

    #[test]
    fn reset_disables_msi() {
        let mut dev = XhciPciDevice::default();

        let msi_off = dev.config_mut().find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
        // Enable MSI.
        let ctrl = dev.config_mut().read(msi_off + 0x02, 2) as u16;
        dev.config_mut()
            .write(msi_off + 0x02, 2, u32::from(ctrl | 0x0001));
        assert!(dev
            .config_mut()
            .capability::<crate::pci::MsiCapability>()
            .is_some_and(|msi| msi.enabled()));

        dev.reset();

        assert!(dev
            .config_mut()
            .capability::<crate::pci::MsiCapability>()
            .is_some_and(|msi| !msi.enabled()));
    }

    #[test]
    fn config_matches_profile() {
        let dev = XhciPciDevice::default();
        let prof = profile::USB_XHCI_QEMU;

        let id = dev.config.vendor_device_id();
        assert_eq!(id.vendor_id, prof.vendor_id);
        assert_eq!(id.device_id, prof.device_id);

        assert_eq!(
            dev.config.class_code(),
            PciClassCode {
                class: prof.class.base_class,
                subclass: prof.class.sub_class,
                prog_if: prof.class.prog_if,
                revision_id: prof.revision_id,
            }
        );

        assert_eq!(
            dev.config.bar_definition(XhciPciDevice::MMIO_BAR_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: XhciPciDevice::MMIO_BAR_SIZE,
                prefetchable: false,
            })
        );
        assert_eq!(
            u64::from(XhciPciDevice::MMIO_BAR_SIZE),
            profile::XHCI_MMIO_BAR_SIZE,
            "xHCI BAR0 size must match the canonical PCI profile"
        );
    }

    #[test]
    fn bar0_probe_returns_expected_size_mask() {
        let mut dev = XhciPciDevice::default();
        let cfg = dev.config_mut();

        // Standard PCI BAR sizing probe: write all 1s then read back the mask.
        cfg.write(0x10, 4, 0xffff_ffff);
        let mask = cfg.read(0x10, 4);

        // MMIO32 BAR, non-prefetchable, size 0x10000 => mask 0xffff_0000.
        let expected = !(XhciPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0;
        assert_eq!(mask, expected);
    }

    #[test]
    fn mmio_reads_return_all_ones_when_mem_decoding_disabled() {
        let mut dev = XhciPciDevice::default();

        // Program BAR0 base but leave COMMAND.MEM cleared.
        dev.config_mut()
            .set_bar_base(XhciPciDevice::MMIO_BAR_INDEX, 0x1000_0000);
        assert_eq!(MmioHandler::read(&mut dev, 0x00, 4), 0xffff_ffff);

        // Enable MEM decoding and verify the capability dword becomes visible.
        dev.config_mut().set_command(0x2);
        assert_eq!(
            MmioHandler::read(&mut dev, 0x00, 4) as u32,
            regs::CAPLENGTH_HCIVERSION
        );
    }

    #[test]
    fn bme_gates_dma_on_run() {
        #[derive(Default)]
        struct Counts {
            reads: usize,
        }

        struct CountingBus {
            counts: Rc<RefCell<Counts>>,
        }

        impl memory::MemoryBus for CountingBus {
            fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
                self.counts.borrow_mut().reads += 1;
                buf.fill(0);
            }

            fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
        }

        let counts = Rc::new(RefCell::new(Counts::default()));
        let bus: Rc<RefCell<dyn memory::MemoryBus>> = Rc::new(RefCell::new(CountingBus {
            counts: counts.clone(),
        }));

        let mut dev = XhciPciDevice::default();
        dev.set_dma_memory_bus(Some(bus));
        dev.config_mut().set_command(1 << 1); // MEM

        // Point CRCR somewhere arbitrary; the controller ignores the contents but should touch the
        // DMA bus on RUN transitions when BME is enabled.
        MmioHandler::write(&mut dev, regs::REG_CRCR_LO, 4, 0x1000);
        MmioHandler::write(&mut dev, regs::REG_CRCR_HI, 4, 0);

        // BME disabled: RUN should not touch the DMA bus (NoDma adapter).
        MmioHandler::write(&mut dev, regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
        assert_eq!(counts.borrow().reads, 0);

        // Stop + enable bus mastering, then start again -> DMA should now be reachable.
        MmioHandler::write(&mut dev, regs::REG_USBCMD, 4, 0);
        dev.config_mut().set_command((1 << 1) | (1 << 2)); // MEM | BME
        MmioHandler::write(&mut dev, regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
        assert!(
            counts.borrow().reads > 0,
            "controller should touch the DMA bus when BME is enabled"
        );
    }

    #[test]
    fn intx_disable_gates_irq_level() {
        let mut dev = XhciPciDevice::default();
        dev.config_mut().set_command(1 << 1); // MEM

        // Trigger the controller's interrupt condition.
        MmioHandler::write(&mut dev, regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
        assert!(dev.irq_level());

        // Disable INTx via PCI command bit 10.
        let command = dev.config().command();
        dev.config_mut().set_command(command | (1 << 10));
        assert!(!dev.irq_level());
    }

    #[test]
    fn snapshot_roundtrip_includes_controller_state() {
        let mut dev = XhciPciDevice::default();
        dev.config_mut().set_command(1 << 1); // MEM

        MmioHandler::write(&mut dev, regs::REG_CRCR_LO, 4, 0x1234);
        MmioHandler::write(&mut dev, regs::REG_CRCR_HI, 4, 0);
        MmioHandler::write(&mut dev, regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

        // CRCR has low control/reserved bits; the controller model may mask/normalize them. Capture
        // the effective visible value and ensure snapshot restore preserves it.
        let expected_crcr = MmioHandler::read(&mut dev, regs::REG_CRCR_LO, 4) as u32;

        let snap = dev.save_state();

        let mut restored = XhciPciDevice::default();
        restored.load_state(&snap).expect("load snapshot");

        let usbcmd = MmioHandler::read(&mut restored, regs::REG_USBCMD, 4) as u32;
        let crcr = MmioHandler::read(&mut restored, regs::REG_CRCR_LO, 4) as u32;
        assert_eq!(usbcmd & regs::USBCMD_RUN, regs::USBCMD_RUN);
        assert_eq!(crcr, expected_crcr);
        assert!(restored.irq_level());
    }
}
