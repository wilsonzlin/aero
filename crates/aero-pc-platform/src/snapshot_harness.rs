use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::{
    CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, Result, SnapshotError, SnapshotMeta,
    SnapshotSource, SnapshotTarget,
};

use crate::PcPlatform;

/// `aero-snapshot` adapter for [`PcPlatform`].
///
/// This is intended as a platform-only harness for integration tests: it snapshots
/// the core chipset devices and guest RAM without requiring a CPU/MMU implementation.
///
/// CPU/MMU state is populated with dummy defaults; tests should focus on `DEVICES`
/// + `RAM` correctness.
pub struct PcPlatformSnapshotHarness<'a> {
    pc: &'a mut PcPlatform,
    restore_error: Option<SnapshotError>,
    restored_interrupts: bool,
    restored_pci_intx: bool,
}

impl<'a> PcPlatformSnapshotHarness<'a> {
    pub fn new(pc: &'a mut PcPlatform) -> Self {
        Self {
            pc,
            restore_error: None,
            restored_interrupts: false,
            restored_pci_intx: false,
        }
    }

    pub fn platform(&mut self) -> &mut PcPlatform {
        self.pc
    }
}

impl SnapshotSource for PcPlatformSnapshotHarness<'_> {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        // Deterministic meta so repeated saves are stable.
        SnapshotMeta::default()
    }

    fn cpu_state(&self) -> CpuState {
        let mut cpu = CpuState::default();
        cpu.a20_enabled = self.pc.memory.a20().enabled();
        cpu
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        let pc = &*self.pc;

        vec![
            device_state_from_io_snapshot(DeviceId::PLATFORM_INTERRUPTS, &*pc.interrupts.borrow()),
            device_state_from_io_snapshot(DeviceId::PIT, &*pc.pit.borrow()),
            device_state_from_io_snapshot(DeviceId::RTC, &*pc.rtc.borrow()),
            device_state_from_io_snapshot(DeviceId::HPET, &*pc.hpet.borrow()),
            device_state_from_io_snapshot(DeviceId::PCI_CFG, &*pc.pci_cfg.borrow()),
            device_state_from_io_snapshot(DeviceId::PCI_INTX_ROUTER, &pc.pci_intx),
        ]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        // `PcPlatform` is constructed from a `usize` RAM length, so this should always fit.
        self.pc.memory.ram().size() as usize
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let ram = self.pc.memory.ram();
        let total_len = ram.size();
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(SnapshotError::Corrupt("ram read overflow"))?;
        if end > total_len {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
        }

        // Important: bypass `MemoryBus::read_physical`, which applies A20 gating.
        ram.read_into(offset, buf)
            .map_err(|_| SnapshotError::Corrupt("ram read failed"))?;
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

impl SnapshotTarget for PcPlatformSnapshotHarness<'_> {
    fn restore_cpu_state(&mut self, state: CpuState) {
        self.pc.memory.a20().set_enabled(state.a20_enabled);
    }

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        // Restore the interrupt sink first. Some devices require a post-restore sync step
        // that must run after the sink is restored (e.g. PCI INTx).
        for state in &states {
            if state.id == DeviceId::PLATFORM_INTERRUPTS {
                if apply_io_snapshot_to_device(state, &mut *self.pc.interrupts.borrow_mut())
                    .is_err()
                {
                    self.restore_error = Some(SnapshotError::Corrupt(
                        "failed to restore platform interrupts state",
                    ));
                    return;
                }
                self.restored_interrupts = true;
                break;
            }
        }

        for state in states {
            match state.id {
                id if id == DeviceId::PLATFORM_INTERRUPTS => {
                    // Already restored above.
                }
                id if id == DeviceId::PIT => {
                    if apply_io_snapshot_to_device(&state, &mut *self.pc.pit.borrow_mut()).is_err()
                    {
                        self.restore_error =
                            Some(SnapshotError::Corrupt("failed to restore PIT state"));
                        return;
                    }
                }
                id if id == DeviceId::RTC => {
                    if apply_io_snapshot_to_device(&state, &mut *self.pc.rtc.borrow_mut()).is_err()
                    {
                        self.restore_error =
                            Some(SnapshotError::Corrupt("failed to restore RTC state"));
                        return;
                    }
                }
                id if id == DeviceId::HPET => {
                    if apply_io_snapshot_to_device(&state, &mut *self.pc.hpet.borrow_mut())
                        .is_err()
                    {
                        self.restore_error =
                            Some(SnapshotError::Corrupt("failed to restore HPET state"));
                        return;
                    }
                }
                id if id == DeviceId::PCI_CFG => {
                    if apply_io_snapshot_to_device(&state, &mut *self.pc.pci_cfg.borrow_mut())
                        .is_err()
                    {
                        self.restore_error = Some(SnapshotError::Corrupt(
                            "failed to restore PCI config ports state",
                        ));
                        return;
                    }
                }
                id if id == DeviceId::PCI_INTX_ROUTER => {
                    if apply_io_snapshot_to_device(&state, &mut self.pc.pci_intx).is_err() {
                        self.restore_error =
                            Some(SnapshotError::Corrupt("failed to restore PCI INTx state"));
                        return;
                    }
                    self.restored_pci_intx = true;
                }
                _ => {}
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.pc.memory.ram().size() as usize
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let ram_len = self.pc.memory.ram().size();
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(SnapshotError::Corrupt("ram write overflow"))?;
        if end > ram_len {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }

        // Important: bypass `MemoryBus::write_physical`, which applies A20 gating.
        self.pc
            .memory
            .ram_mut()
            .write_from(offset, data)
            .map_err(|_| SnapshotError::Corrupt("ram write failed"))?;
        Ok(())
    }

    fn post_restore(&mut self) -> Result<()> {
        if let Some(err) = self.restore_error.take() {
            return Err(err);
        }

        // `PciIntxRouter::load_state()` cannot drive the platform interrupt sink. Re-drive the
        // restored INTx levels into the sink after both sides have been restored.
        if self.restored_pci_intx {
            if !self.restored_interrupts {
                return Err(SnapshotError::Corrupt(
                    "PCI INTx state restored without platform interrupts state",
                ));
            }
            self.pc
                .pci_intx
                .sync_levels_to_sink(&mut *self.pc.interrupts.borrow_mut());
        }

        Ok(())
    }
}
