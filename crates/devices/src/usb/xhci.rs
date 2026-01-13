//! xHCI (USB 3.x) controller exposed as a PCI function.
//!
//! This module currently focuses on PCI-level plumbing:
//! - PCI config space identity
//! - legacy INTx level signalling (`irq_level`)
//! - MSI delivery when the guest enables the MSI capability
//!
//! The actual xHCI register model is not implemented here; this wrapper provides just enough
//! structure for platform integrations to wire up a native (host-backed) xHCI implementation and
//! for unit/integration tests to validate interrupt delivery behaviour.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::interrupts::msi::MsiTrigger;

use crate::irq::IrqLine;
use crate::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use crate::pci::{profile, MsiCapability, PciConfigSpace, PciConfigSpaceState, PciDevice};

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
    irq: AtomicIrqLine,
    msi_target: Option<Box<dyn MsiTrigger>>,
    last_irq_level: bool,
}

impl XhciPciDevice {
    /// Create a new xHCI PCI device wrapper with a default config-space identity.
    pub fn new() -> Self {
        let irq = AtomicIrqLine::default();

        // Use a simple, deterministic profile for the PCI identity. The exact vendor/device ID is
        // not critical for the interrupt-plumbing tests; platform integrations may override this
        // as needed once a full xHCI model is wired up.
        let mut config = PciConfigSpace::new(profile::PCI_VENDOR_ID_INTEL, 0x1e31);
        // xHCI class code: Serial Bus / USB / xHCI (prog-if 0x30).
        config.set_class_code(0x0c, 0x03, 0x30, 0x00);

        // Expose a single-vector MSI capability so guests can opt into message-signaled
        // interrupts. MSI-X can be added later when a full xHCI implementation is available.
        config.add_capability(Box::new(MsiCapability::new()));

        Self {
            config,
            irq,
            msi_target: None,
            last_irq_level: false,
        }
    }

    /// Configure the target used for MSI interrupt delivery.
    ///
    /// Platform integrations should provide a sink that injects the programmed MSI message into the
    /// guest (e.g. `PlatformInterrupts` in APIC mode).
    ///
    /// If no target is configured, the device falls back to legacy INTx signalling even when the
    /// guest enables MSI in PCI config space. This preserves compatibility with platforms that do
    /// not support MSI delivery (e.g. the Web runtime).
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
        let level = self.irq.level();

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

        self.irq.level()
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
        self.irq.set_level(false);
        self.last_irq_level = false;
    }
}

impl IoSnapshot for XhciPciDevice {
    const DEVICE_ID: [u8; 4] = *b"XHCP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_IRQ: u16 = 2;
        const TAG_LAST_IRQ: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let pci = self.config.snapshot_state();
        let mut pci_enc = Encoder::new().bytes(&pci.bytes);
        for i in 0..6 {
            pci_enc = pci_enc.u64(pci.bar_base[i]).bool(pci.bar_probe[i]);
        }
        w.field_bytes(TAG_PCI, pci_enc.finish());

        w.field_bytes(TAG_IRQ, Encoder::new().bool(self.irq.level()).finish());
        w.field_bytes(TAG_LAST_IRQ, Encoder::new().bool(self.last_irq_level).finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_IRQ: u16 = 2;
        const TAG_LAST_IRQ: u16 = 3;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

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
        } else {
            self.irq.set_level(false);
        }

        if let Some(buf) = r.bytes(TAG_LAST_IRQ) {
            let mut d = Decoder::new(buf);
            self.last_irq_level = d.bool()?;
            d.finish()?;
        } else {
            // Older snapshots (or minimal ones) default to the restored IRQ level to avoid
            // spuriously generating an MSI edge immediately after restore.
            self.last_irq_level = self.irq.level();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::XhciPciDevice;
    use crate::pci::msi::PCI_CAP_ID_MSI;
    use crate::pci::PciDevice;

    #[test]
    fn exposes_msi_capability() {
        let mut dev = XhciPciDevice::default();
        assert!(
            dev.config_mut().find_capability(PCI_CAP_ID_MSI).is_some(),
            "xHCI device should expose an MSI capability"
        );
    }
}
