use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use aero_devices::irq::IrqLine;
use aero_devices::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use aero_devices::pci::{profile, MsiCapability, PciConfigSpace, PciConfigSpaceState, PciDevice};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::interrupts::msi::MsiTrigger;
use memory::MemoryBus;
use memory::MmioHandler;

use crate::ahci::AhciController;
use crate::ata::AtaDrive;

/// PCI BAR index used for AHCI ABAR on Intel ICH9.
///
/// This is sourced from the canonical PCI device profile in `aero-devices` so the storage device
/// model and the guest-facing PCI identity cannot drift.
pub const AHCI_ABAR_BAR_INDEX: u8 = aero_devices::pci::profile::AHCI_ABAR_BAR_INDEX;

/// PCI config space offset of the AHCI ABAR register (BAR5 on Intel ICH9).
pub const AHCI_ABAR_CFG_OFFSET: u8 = aero_devices::pci::profile::AHCI_ABAR_CFG_OFFSET;

/// AHCI ABAR (HBA registers) size in bytes as a `u32` (for `PciBarDefinition::Mmio32`).
pub const AHCI_ABAR_SIZE_U32: u32 = aero_devices::pci::profile::AHCI_ABAR_SIZE_U32;

/// AHCI ABAR (HBA registers) size in bytes.
pub const AHCI_ABAR_SIZE: u64 = aero_devices::pci::profile::AHCI_ABAR_SIZE;

const PCI_COMMAND_MEM_ENABLE: u16 = 1 << 1;

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
    fn set_level(&self, high: bool) {
        self.level.store(high, Ordering::SeqCst);
    }
}

/// Intel ICH9 AHCI controller exposed as a PCI device.
///
/// This is a thin wrapper around [`AhciController`] that:
/// - exposes an ICH9-like PCI config space (`aero_devices::pci::profile::SATA_AHCI_ICH9`)
/// - provides BAR5 (ABAR) MMIO accessors for HBA/port registers
/// - tracks legacy INTx level for platform routing
/// - bridges DMA through [`memory::MemoryBus`]
pub struct AhciPciDevice {
    config: PciConfigSpace,
    controller: AhciController,
    irq: AtomicIrqLine,
    msi_target: Option<Box<dyn MsiTrigger>>,
    last_irq_level: bool,
}

impl AhciPciDevice {
    /// Creates a new AHCI PCI device with the given number of ports.
    ///
    /// Port count must be within the AHCI architectural limit (1..=32).
    pub fn new(num_ports: usize) -> Self {
        let irq = AtomicIrqLine::default();
        let controller = AhciController::new(Box::new(irq.clone()), num_ports);
        let config = profile::SATA_AHCI_ICH9.build_config_space();
        Self {
            config,
            controller,
            irq,
            msi_target: None,
            last_irq_level: false,
        }
    }

    /// Configure the target used for MSI interrupt delivery.
    ///
    /// Platform integrations should provide a sink that injects the programmed MSI message into the
    /// guest LAPIC (e.g. `PlatformInterrupts` in APIC mode).
    ///
    /// When no target is configured, the device falls back to legacy INTx signalling even if the
    /// guest enables MSI in PCI config space.
    pub fn set_msi_target(&mut self, target: Option<Box<dyn MsiTrigger>>) {
        self.msi_target = target;
    }

    pub fn attach_drive(&mut self, port: usize, drive: AtaDrive) {
        self.controller.attach_drive(port, drive);
    }

    pub fn detach_drive(&mut self, port: usize) {
        self.controller.detach_drive(port);
    }

    /// Reset the device back to its power-on state while preserving attached drives.
    ///
    /// This is intended for machine/platform reset flows where host-provided disk backends should
    /// remain attached across reboots.
    pub fn reset(&mut self) {
        // Mirror PCI reset semantics: clear command register state (BAR programming is preserved).
        //
        // Note: We implement `PciDevice::reset` for this type by calling this method, so avoid
        // calling the trait method here (it would recurse).
        self.config.set_command(0);
        self.config.disable_msi_msix();
        self.controller.reset();
        self.last_irq_level = self.irq.level();
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

        // MSI delivery is edge-triggered; fire only on a rising edge of the internal interrupt
        // condition. (INTx remains level-triggered via `intx_level()`.)
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
    /// This is already gated by the PCI Command register's Interrupt Disable bit (bit 10).
    pub fn intx_level(&self) -> bool {
        // When MSI is enabled and the platform provided an MSI sink, suppress legacy INTx so
        // interrupts are not delivered twice.
        if self.msi_active() {
            return false;
        }

        // PCI command bit 10 disables legacy INTx assertion.
        let intx_disabled = (self.config.command() & (1 << 10)) != 0;
        if intx_disabled {
            return false;
        }
        self.irq.level()
    }

    /// Reads from the AHCI ABAR MMIO region.
    pub fn mmio_read(&mut self, offset: u64, size: usize) -> u64 {
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
            if byte_off >= AHCI_ABAR_SIZE {
                // Unmapped bytes read as 0xFF on the platform memory bus; mirror that here.
                out |= 0xFFu64 << (i * 8);
                continue;
            }

            let word_off = byte_off & !3;
            let shift = ((byte_off & 3) * 8) as u32;
            let word = self.controller.read_u32(word_off);
            let byte = ((word >> shift) & 0xFF) as u64;
            out |= byte << (i * 8);
        }

        out
    }

    /// Writes to the AHCI ABAR MMIO region.
    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64) {
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
            if byte_off >= AHCI_ABAR_SIZE {
                idx += 1;
                continue;
            }

            let word_off = byte_off & !3;

            // Collect the bytes that fall into this 32-bit word.
            let mut be_mask = 0u32;
            let mut write_val = 0u32;
            while idx < size {
                let off = match offset.checked_add(idx as u64) {
                    Some(v) => v,
                    None => break,
                };
                if (off & !3) != word_off {
                    break;
                }
                if off >= AHCI_ABAR_SIZE {
                    idx += 1;
                    continue;
                }

                let byte_idx_in_word = (off & 3) as usize;
                let shift = (byte_idx_in_word * 8) as u32;
                let byte = ((value >> (idx * 8)) & 0xFF) as u32;
                write_val |= byte << shift;
                be_mask |= 0xFFu32 << shift;
                idx += 1;
            }

            if be_mask == 0 {
                continue;
            }

            if is_w1c_register(word_off) {
                // For W1C registers, only the written bytes should have an effect. Unwritten bytes
                // behave as zeros (no-op).
                self.controller.write_u32(word_off, write_val);
                continue;
            }

            // For regular registers, implement byte enables via read-modify-write.
            let current = self.controller.read_u32(word_off);
            let merged = (current & !be_mask) | (write_val & be_mask);
            self.controller.write_u32(word_off, merged);
        }

        // MMIO writes may change interrupt enable/status bits; service MSI edge detection after the
        // operation completes.
        self.service_interrupts();
    }

    /// Processes pending AHCI commands (DMA) using the provided guest physical memory bus.
    ///
    /// This should be called by a platform when:
    /// - the guest sets bits in PxCI, and/or
    /// - on a periodic tick to model asynchronous device progress.
    ///
    /// DMA is only performed when PCI Bus Mastering is enabled (PCI Command bit 2).
    pub fn process(&mut self, mem: &mut dyn MemoryBus) {
        let bus_master_enabled = (self.config.command() & (1 << 2)) != 0;
        if !bus_master_enabled {
            return;
        }

        self.controller.process(mem);
        self.service_interrupts();
    }
}

fn is_w1c_register(offset: u64) -> bool {
    // HBA.IS
    if offset == 0x08 {
        return true;
    }

    // Per-port registers: PxIS (0x10) and PxSERR (0x30).
    if offset < 0x100 {
        return false;
    }
    let port_reg_off = (offset - 0x100) % 0x80;
    matches!(port_reg_off, 0x10 | 0x30)
}

impl PciDevice for AhciPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        AhciPciDevice::reset(self);
    }
}

impl MmioHandler for AhciPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.mmio_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.mmio_write(offset, size, value);
    }
}

impl IoSnapshot for AhciPciDevice {
    const DEVICE_ID: [u8; 4] = *b"AHCP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;
        const TAG_LAST_IRQ_LEVEL: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let pci = self.config.snapshot_state();
        let mut pci_enc = Encoder::new().bytes(&pci.bytes);
        for i in 0..6 {
            pci_enc = pci_enc.u64(pci.bar_base[i]).bool(pci.bar_probe[i]);
        }
        w.field_bytes(TAG_PCI, pci_enc.finish());

        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());

        w.field_bytes(
            TAG_LAST_IRQ_LEVEL,
            Encoder::new().bool(self.last_irq_level).finish(),
        );

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;
        const TAG_LAST_IRQ_LEVEL: u16 = 3;

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

        let Some(buf) = r.bytes(TAG_CONTROLLER) else {
            return Err(SnapshotError::InvalidFieldEncoding(
                "missing ahci controller state",
            ));
        };
        self.controller.load_state(buf)?;

        if let Some(buf) = r.bytes(TAG_LAST_IRQ_LEVEL) {
            let mut d = Decoder::new(buf);
            self.last_irq_level = d.bool()?;
            d.finish()?;
        } else {
            // Older snapshots didn't include edge-tracking state for MSI. Default to the restored
            // controller's current interrupt condition so restore does not spuriously generate an
            // MSI edge on the next tick.
            self.last_irq_level = self.irq.level();
        }

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

    const HBA_REG_IS: u64 = 0x08;
    const PORT0_BASE: u64 = 0x100;
    const PORT_REG_CLB: u64 = 0x00;
    const PORT_REG_FB: u64 = 0x08;
    const PORT_REG_IS: u64 = 0x10;
    const PORT_REG_IE: u64 = 0x14;
    const PORT_REG_SERR: u64 = 0x30;

    #[test]
    fn mmio_read_size0_returns_zero() {
        let mut dev = AhciPciDevice::new(1);

        // Even if the PCI function is not enabled for MMIO decoding, a size-0 access is a no-op.
        assert_eq!(dev.mmio_read(0, 0), 0);
    }

    #[test]
    fn mmio_write_size0_is_noop_and_does_not_clear_w1c_or_irq() {
        let mut dev = AhciPciDevice::new(1);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // Synthesize an asserted interrupt by setting PxIS + PxIE with GHC.IE enabled.
        let mut state = dev.controller.snapshot_state();
        state.hba.ghc |= 1 << 1; // GHC.IE
        state.ports[0].is = 1; // PxIS.DHRS
        state.ports[0].ie = 1; // PxIE.DHRE (enable)
        dev.controller.restore_state(&state);

        assert!(
            dev.intx_level(),
            "interrupt should be asserted before the write"
        );

        let px_is_off = 0x100 + 0x10; // PxIS for port 0.
        let before = dev.mmio_read(px_is_off, 4);
        assert_eq!(before, 1);

        // Regression: previously `size=0` was clamped to 1, which would perform a 1-byte W1C write
        // and clear the interrupt status/level.
        dev.mmio_write(px_is_off, 0, 0xff);

        let after = dev.mmio_read(px_is_off, 4);
        assert_eq!(after, before, "size-0 MMIO write must not change PxIS");
        assert!(
            dev.intx_level(),
            "size-0 MMIO write must not change IRQ level"
        );
    }

    #[test]
    fn mmio_write_byte_enable_rmw_preserves_unwritten_bytes_for_normal_register() {
        let mut dev = AhciPciDevice::new(1);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // PxIE is a normal read/write register (not W1C). Sub-dword accesses should behave like
        // byte-enable semantics: read-modify-write so unwritten bytes are preserved.
        let px_ie_off = 0x100 + 0x14; // PxIE for port 0.
        let initial = 0xA1B2_C3D4u32;
        dev.mmio_write(px_ie_off, 4, initial as u64);
        assert_eq!(dev.mmio_read(px_ie_off, 4), initial as u64);

        // Overwrite only byte lane 1 (bits 8..15).
        dev.mmio_write(px_ie_off + 1, 1, 0xEE);
        let expected = (initial & !0x0000_FF00) | (0xEEu32 << 8);
        assert_eq!(dev.mmio_read(px_ie_off, 4), expected as u64);
    }

    #[test]
    fn mmio_write_byte_enable_w1c_unwritten_bytes_are_noop() {
        let mut dev = AhciPciDevice::new(1);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // Seed PxIS with bits set across multiple bytes, then clear only a single bit via a
        // sub-dword write. Unwritten bytes must be treated as zero (no-op), not as a preserved
        // value (RMW) or as 0xFF.
        let px_is_off = 0x100 + 0x10; // PxIS for port 0.
        let mut state = dev.controller.snapshot_state();
        state.ports[0].is = 0x0101_0101;
        dev.controller.restore_state(&state);

        assert_eq!(dev.mmio_read(px_is_off, 4), 0x0101_0101);

        // Clear only bit8 (byte lane 1) with a 2-byte write, leaving bit0 and higher bytes intact.
        dev.mmio_write(px_is_off, 2, 0x0100);
        assert_eq!(dev.mmio_read(px_is_off, 4), 0x0101_0001);
    }

    #[test]
    fn mmio_write_partial_rmw_byte_enables_for_non_w1c_register() {
        let mut dev = AhciPciDevice::new(1);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        let px_ie_off = PORT0_BASE + PORT_REG_IE;

        // Initialize with a known dword value.
        dev.mmio_write(px_ie_off, 4, 0x1122_3344);
        assert_eq!(dev.mmio_read(px_ie_off, 4), 0x1122_3344);

        // 1-byte write should only affect the targeted byte (byte enables via RMW).
        dev.mmio_write(px_ie_off + 1, 1, 0xAA);
        assert_eq!(
            dev.mmio_read(px_ie_off, 4),
            0x1122_AA44,
            "1-byte write must not clobber other bytes"
        );

        // 2-byte write should only affect the targeted bytes.
        dev.mmio_write(px_ie_off + 2, 2, 0xBEEF);
        assert_eq!(
            dev.mmio_read(px_ie_off, 4),
            0xBEEF_AA44,
            "2-byte write must not clobber other bytes"
        );
    }

    #[test]
    fn mmio_write_w1c_hba_is_byte_writes_only_clear_written_bytes() {
        // HBA.IS is a W1C register, but unlike regular registers it must *not* use RMW to implement
        // byte enables: unwritten bytes should behave as zeros (no effect).
        let mut dev = AhciPciDevice::new(16);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // Synthesize interrupt status for port 0 and port 8 (different bytes in HBA.IS).
        let mut state = dev.controller.snapshot_state();
        state.ports[0].is = 1;
        state.ports[8].is = 1;
        dev.controller.restore_state(&state);

        assert_eq!(dev.mmio_read(HBA_REG_IS, 4) as u32, (1 << 0) | (1 << 8));

        // Clear port 0 summary bit via a byte write to byte 0.
        dev.mmio_write(HBA_REG_IS, 1, 1);
        assert_eq!(
            dev.mmio_read(HBA_REG_IS, 4) as u32,
            1 << 8,
            "byte write to HBA.IS[0] must not clear bits in other bytes"
        );

        // Reset status bits and clear port 8 via a byte write to byte 1.
        let mut state = dev.controller.snapshot_state();
        state.ports[0].is = 1;
        state.ports[8].is = 1;
        dev.controller.restore_state(&state);

        dev.mmio_write(HBA_REG_IS + 1, 1, 1);
        assert_eq!(
            dev.mmio_read(HBA_REG_IS, 4) as u32,
            1 << 0,
            "byte write to HBA.IS[1] must not clear bits in other bytes"
        );
    }

    #[test]
    fn mmio_write_w1c_px_is_and_px_serr_byte_enables_do_not_use_rmw() {
        let mut dev = AhciPciDevice::new(1);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // Seed port status registers with bits set in multiple bytes so we can detect accidental
        // clearing from an incorrect RMW merge.
        let mut state = dev.controller.snapshot_state();
        state.ports[0].is = 0xFFFF_FFFF;
        state.ports[0].serr = 0xFFFF_FFFF;
        dev.controller.restore_state(&state);

        let px_is_off = PORT0_BASE + PORT_REG_IS;
        let px_serr_off = PORT0_BASE + PORT_REG_SERR;

        assert_eq!(dev.mmio_read(px_is_off, 4), 0xFFFF_FFFF);
        assert_eq!(dev.mmio_read(px_serr_off, 4), 0xFFFF_FFFF);

        // Clear the second byte of PxIS with a byte write.
        dev.mmio_write(px_is_off + 1, 1, 0xFF);
        assert_eq!(
            dev.mmio_read(px_is_off, 4),
            0xFFFF_00FF,
            "byte write must only clear bits covered by the written byte"
        );

        // Clear the upper 2 bytes of PxIS with a 2-byte write.
        dev.mmio_write(px_is_off + 2, 2, 0xFFFF);
        assert_eq!(
            dev.mmio_read(px_is_off, 4),
            0x0000_00FF,
            "2-byte write must only clear bits covered by the written bytes"
        );

        // Clear the high byte of PxSERR with a byte write.
        dev.mmio_write(px_serr_off + 3, 1, 0xFF);
        assert_eq!(
            dev.mmio_read(px_serr_off, 4),
            0x00FF_FFFF,
            "byte write must only clear bits covered by the written byte"
        );
    }

    #[test]
    fn mmio_out_of_range_write_is_ignored_and_size_is_clamped() {
        let mut dev = AhciPciDevice::new(1);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // Initialize a register we can observe.
        let px_ie_off = PORT0_BASE + PORT_REG_IE;
        dev.mmio_write(px_ie_off, 4, 0x1234_5678);
        assert_eq!(dev.mmio_read(px_ie_off, 4), 0x1234_5678);

        // Writes beyond the ABAR window should be ignored.
        dev.mmio_write(AHCI_ABAR_SIZE + 16, 4, 0xFFFF_FFFF);
        assert_eq!(
            dev.mmio_read(px_ie_off, 4),
            0x1234_5678,
            "out-of-range write must not affect in-range registers"
        );

        // Oversized accesses should be clamped to 8 bytes. Verify that a size-16 write only
        // updates the first 8 bytes (CLB/CLBU) and does not touch the adjacent FB/FBU.
        let px_clb_off = PORT0_BASE + PORT_REG_CLB;
        let px_fb_off = PORT0_BASE + PORT_REG_FB;

        dev.mmio_write(px_fb_off, 8, 0xA1A2_A3A4_A5A6_A7A8);
        let fb_before = dev.mmio_read(px_fb_off, 8);

        dev.mmio_write(px_clb_off, 16, 0x1122_3344_5566_7788);
        assert_eq!(dev.mmio_read(px_clb_off, 8), 0x1122_3344_5566_7788);
        assert_eq!(
            dev.mmio_read(px_fb_off, 8),
            fb_before,
            "size clamping must prevent writes beyond 8 bytes"
        );
    }

    #[test]
    fn reset_disables_msi_enable_bit() {
        let mut dev = AhciPciDevice::new(1);
        let cap_offset = dev
            .config_mut()
            .find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI)
            .expect("AHCI device should expose an MSI capability") as u16;

        // Enable MSI.
        {
            let cfg = dev.config_mut();
            let ctrl = cfg.read(cap_offset + 0x02, 2) as u16;
            cfg.write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));
            assert!(cfg.capability::<MsiCapability>().unwrap().enabled());
        }

        dev.reset();

        assert!(
            !dev.config_mut().capability::<MsiCapability>().unwrap().enabled(),
            "MSI must be disabled after PCI device reset"
        );
    }
}
