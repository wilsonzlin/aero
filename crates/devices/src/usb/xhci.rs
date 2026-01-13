//! xHCI (USB 3.x) controller exposed as a PCI function.
//!
//! This module is intentionally minimal: it provides a PCI wrapper plus a small BAR0 MMIO register
//! backing store so platform integrations can expose an xHCI controller as a PCI MMIO device and
//! validate enumeration/interrupt wiring.
//!
//! Today it includes:
//! - QEMU-compatible PCI identity (`profile::USB_XHCI_QEMU`)
//! - BAR0 MMIO decoding via a simple read/write byte array (enough for smoke tests)
//! - legacy INTx level signalling (`irq_level`) with COMMAND.INTX_DISABLE gating
//! - optional single-vector MSI delivery when the guest enables MSI
//! - snapshot/restore of PCI config + interrupt latch state
//!
//! A full xHCI register model is not implemented yet.

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
use memory::{MemoryBus as _, MmioHandler};

use crate::irq::IrqLine;
use crate::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use crate::pci::{profile, MsiCapability, PciBarKind, PciConfigSpace, PciConfigSpaceState, PciDevice};

// Minimal xHCI register offsets / bits used by the platform integration tests.
//
// We keep these local to avoid coupling `aero-devices` to the canonical xHCI register model living
// in `aero-usb`.
const REG_USBCMD: usize = 0x40;
const REG_USBSTS: usize = 0x44;
const REG_CRCR: usize = 0x58;

const USBCMD_RUN: u32 = 1 << 0;
const USBSTS_EINT: u32 = 1 << 3;

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
/// - A minimal BAR0 MMIO register backing store
/// - An internal interrupt condition that can be surfaced via:
///   - legacy INTx (`irq_level()`), or
///   - MSI (`service_interrupts()` when MSI is enabled and a target is configured).
pub struct XhciPciDevice {
    config: PciConfigSpace,
    mmio: Vec<u8>,
    irq: AtomicIrqLine,
    msi_target: Option<Box<dyn MsiTrigger>>,
    last_irq_level: bool,
    run_edge_pending: bool,
}

impl XhciPciDevice {
    /// xHCI MMIO BAR size (BAR0).
    pub const MMIO_BAR_SIZE: u32 = profile::XHCI_MMIO_BAR_SIZE_U32;
    /// xHCI MMIO BAR index (BAR0).
    pub const MMIO_BAR_INDEX: u8 = profile::XHCI_MMIO_BAR_INDEX;

    /// Create a new xHCI PCI device wrapper with a QEMU-compatible PCI identity.
    pub fn new() -> Self {
        let irq = AtomicIrqLine::default();

        // Start from the canonical QEMU-style xHCI PCI profile so BAR definitions and class code are
        // consistent with the guest-visible config-space stub used by `PcPlatform`.
        let config = profile::USB_XHCI_QEMU.build_config_space();

        let mut dev = Self {
            config,
            mmio: vec![0; Self::MMIO_BAR_SIZE as usize],
            irq,
            msi_target: None,
            last_irq_level: false,
            run_edge_pending: false,
        };
        dev.reset_mmio_image();
        dev
    }

    fn mmio_u32(&self, offset: usize) -> u32 {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&self.mmio[offset..offset + 4]);
        u32::from_le_bytes(buf)
    }

    fn mmio_u64(&self, offset: usize) -> u64 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.mmio[offset..offset + 8]);
        u64::from_le_bytes(buf)
    }

    fn set_mmio_u32(&mut self, offset: usize, value: u32) {
        self.mmio[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_usbsts_bits(&mut self, bits: u32) {
        let v = self.mmio_u32(REG_USBSTS) | bits;
        self.set_mmio_u32(REG_USBSTS, v);
    }

    fn clear_usbsts_bits(&mut self, bits: u32) {
        let v = self.mmio_u32(REG_USBSTS) & !bits;
        self.set_mmio_u32(REG_USBSTS, v);
    }

    fn reset_mmio_image(&mut self) {
        self.mmio.fill(0);
        self.run_edge_pending = false;

        // xHCI capability registers (spec 5.3.3).
        //
        // CAPLENGTH (offset 0x00, byte 0): length of the capability register block in bytes.
        // HCIVERSION (offset 0x02, u16): xHCI version number.
        //
        // Initialize these to sane defaults so platform-level smoke tests can locate the
        // operational register block at `BAR0 + CAPLENGTH`.
        if self.mmio.len() >= 4 {
            const CAPLENGTH: u8 = 0x40;
            const HCIVERSION: u16 = 0x0100;
            self.mmio[0] = CAPLENGTH;
            self.mmio[2..4].copy_from_slice(&HCIVERSION.to_le_bytes());
        }
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
        // Expose the interrupt state to guests via the USBSTS.EINT latch. Real xHCI controllers
        // have per-interrupter IMAN/IP modelling; this simplified device ties the interrupt source
        // directly to the global USBSTS bit.
        self.set_usbsts_bits(USBSTS_EINT);
        self.irq.set_level(true);
        self.service_interrupts();
    }

    /// Clears the internal interrupt condition.
    pub fn clear_event_interrupt(&mut self) {
        self.clear_usbsts_bits(USBSTS_EINT);
        self.irq.set_level(false);
        self.service_interrupts();
    }

    fn mmio_decode_enabled(&self) -> bool {
        (self.config.command() & 0x2) != 0
    }

    fn bar0_range(&self) -> Option<(u64, u64)> {
        let range = self.config.bar_range(Self::MMIO_BAR_INDEX)?;
        if !matches!(range.kind, PciBarKind::Mmio32 | PciBarKind::Mmio64) {
            return None;
        }
        Some((range.base, range.size))
    }

    /// Advance the device by 1ms. For now this is used primarily to service MSI edge delivery if
    /// the interrupt condition is toggled by an underlying controller via the shared IRQ line.
    pub fn tick_1ms(&mut self, mem: &mut MemoryBus) {
        if self.run_edge_pending {
            self.run_edge_pending = false;

            // If the controller is still running, perform a single bus-master DMA read and then
            // surface an interrupt via USBSTS.EINT. This is intentionally minimal but gives tests
            // something deterministic to observe.
            if (self.mmio_u32(REG_USBCMD) & USBCMD_RUN) != 0 {
                self.dma_on_run(mem);
                self.raise_event_interrupt();
            }
        }
        self.service_interrupts();
    }

    fn dma_on_run(&mut self, mem: &mut MemoryBus) {
        // Gate DMA on PCI Bus Master Enable (bit 2).
        if (self.config.command() & (1 << 2)) == 0 {
            return;
        }

        let paddr = self.mmio_u64(REG_CRCR);
        let _ = mem.read_u32(paddr);
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
        self.reset_mmio_image();
    }
}

impl MmioHandler for XhciPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if !(1..=8).contains(&size) {
            return all_ones(size);
        }

        // Apply COMMAND.MEM gating when the device is used standalone (outside a platform router).
        if !self.mmio_decode_enabled() {
            return all_ones(size);
        }

        // Treat BAR0 base == 0 as unmapped, matching PCI routing behavior.
        if self.bar0_range().map(|(base, _)| base).unwrap_or(0) == 0 {
            return all_ones(size);
        }

        let offset_usize = match usize::try_from(offset) {
            Ok(v) => v,
            Err(_) => return all_ones(size),
        };
        let end = match offset_usize.checked_add(size) {
            Some(v) => v,
            None => return all_ones(size),
        };
        if end > self.mmio.len() {
            return all_ones(size);
        }

        let mut buf = [0u8; 8];
        buf[..size].copy_from_slice(&self.mmio[offset_usize..end]);
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        if !(1..=8).contains(&size) {
            return;
        }

        if !self.mmio_decode_enabled() {
            return;
        }

        if self.bar0_range().map(|(base, _)| base).unwrap_or(0) == 0 {
            return;
        }

        let offset_usize = match usize::try_from(offset) {
            Ok(v) => v,
            Err(_) => return,
        };
        let end = match offset_usize.checked_add(size) {
            Some(v) => v,
            None => return,
        };
        if end > self.mmio.len() {
            return;
        }

        let usbcmd_range = REG_USBCMD..REG_USBCMD + 4;
        let usbsts_range = REG_USBSTS..REG_USBSTS + 4;

        let overlaps_usbcmd = offset_usize < usbcmd_range.end && end > usbcmd_range.start;
        let overlaps_usbsts = offset_usize < usbsts_range.end && end > usbsts_range.start;

        let prev_usbcmd = overlaps_usbcmd.then(|| self.mmio_u32(REG_USBCMD)).unwrap_or(0);
        let prev_usbsts = overlaps_usbsts.then(|| self.mmio_u32(REG_USBSTS)).unwrap_or(0);

        let bytes = value.to_le_bytes();
        for i in 0..size {
            let idx = offset_usize + i;
            let byte = bytes[i];

            // USBSTS is RW1C: writing 1 clears the bit, writing 0 has no effect.
            if (REG_USBSTS..REG_USBSTS + 4).contains(&idx) {
                self.mmio[idx] &= !byte;
            } else {
                self.mmio[idx] = byte;
            }
        }

        if overlaps_usbcmd {
            let new_usbcmd = self.mmio_u32(REG_USBCMD);
            let was_running = (prev_usbcmd & USBCMD_RUN) != 0;
            let now_running = (new_usbcmd & USBCMD_RUN) != 0;
            if !was_running && now_running {
                self.run_edge_pending = true;
            }
        }

        if overlaps_usbsts {
            let new_usbsts = self.mmio_u32(REG_USBSTS);
            let was_eint = (prev_usbsts & USBSTS_EINT) != 0;
            let now_eint = (new_usbsts & USBSTS_EINT) != 0;
            if was_eint && !now_eint {
                self.clear_event_interrupt();
            }
        }
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

        // MMIO register model is currently minimal and not snapshotted; restore to a deterministic
        // baseline.
        self.reset_mmio_image();

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
    use super::XhciPciDevice;
    use crate::pci::config::PciClassCode;
    use crate::pci::msi::PCI_CAP_ID_MSI;
    use crate::pci::profile::{PCI_DEVICE_ID_QEMU_XHCI, PCI_VENDOR_ID_REDHAT_QEMU};
    use crate::pci::{PciBarDefinition, PciDevice};
    use memory::MmioHandler;

    #[test]
    fn exposes_msi_capability() {
        let mut dev = XhciPciDevice::default();
        assert!(
            dev.config_mut().find_capability(PCI_CAP_ID_MSI).is_some(),
            "xHCI device should expose an MSI capability"
        );
    }

    #[test]
    fn config_matches_profile() {
        let dev = XhciPciDevice::default();

        let id = dev.config.vendor_device_id();
        assert_eq!(
            id.vendor_id, PCI_VENDOR_ID_REDHAT_QEMU,
            "xHCI should use the Red Hat/QEMU vendor ID"
        );
        assert_eq!(
            id.device_id, PCI_DEVICE_ID_QEMU_XHCI,
            "xHCI should use the QEMU xHCI device ID"
        );

        assert_eq!(
            dev.config.class_code(),
            PciClassCode {
                class: 0x0c,
                subclass: 0x03,
                prog_if: 0x30,
                revision_id: 0x01,
            },
            "xHCI class code must be 0x0c0330 (xHCI) with a stable revision ID"
        );

        assert_eq!(
            dev.config.bar_definition(XhciPciDevice::MMIO_BAR_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: XhciPciDevice::MMIO_BAR_SIZE,
                prefetchable: false,
            })
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
        assert_eq!(MmioHandler::read(&mut dev, 0x00, 4) as u32, 0x0100_0040);
    }
}
