use aero_devices::pci::PciCoreSnapshot;
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::{
    CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, Result, SnapshotError,
    SnapshotMeta, SnapshotSource, SnapshotTarget,
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
    restored_hpet: bool,
    restored_pci_intx: bool,
}

impl<'a> PcPlatformSnapshotHarness<'a> {
    pub fn new(pc: &'a mut PcPlatform) -> Self {
        Self {
            pc,
            restore_error: None,
            restored_interrupts: false,
            restored_hpet: false,
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
        CpuState {
            a20_enabled: self.pc.memory.a20().enabled(),
            ..Default::default()
        }
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
            device_state_from_io_snapshot(DeviceId::ACPI_PM, &*pc.acpi_pm.borrow()),
            device_state_from_io_snapshot(DeviceId::PCI_CFG, &*pc.pci_cfg.borrow()),
            device_state_from_io_snapshot(DeviceId::PCI_INTX_ROUTER, &pc.pci_intx),
        ]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        usize::try_from(self.pc.ram_size_bytes).unwrap_or(0)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        const FOUR_GIB: u64 = 0x1_0000_0000;
        let low_ram_end = crate::PCIE_ECAM_BASE;
        let ram = self.pc.memory.ram();
        let total_len = self.pc.ram_size_bytes;
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(SnapshotError::Corrupt("ram read overflow"))?;
        if end > total_len {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
        }

        // Important: bypass `MemoryBus::read_physical`, which applies A20 gating.
        //
        // Snapshots encode RAM as a dense byte array of length `ram_size_bytes` (not including any
        // guest-physical MMIO holes). When RAM is remapped above 4GiB to make room for the PCIe
        // ECAM/PCI hole, translate dense RAM offsets into the corresponding guest-physical
        // addresses.
        if total_len <= low_ram_end || buf.is_empty() {
            ram.read_into(offset, buf)
                .map_err(|_| SnapshotError::Corrupt("ram read failed"))?;
            return Ok(());
        }

        if offset < low_ram_end {
            let low_len = (low_ram_end - offset) as usize;
            let first = low_len.min(buf.len());
            ram.read_into(offset, &mut buf[..first])
                .map_err(|_| SnapshotError::Corrupt("ram read failed"))?;
            if first < buf.len() {
                ram.read_into(FOUR_GIB, &mut buf[first..])
                    .map_err(|_| SnapshotError::Corrupt("ram read failed"))?;
            }
            return Ok(());
        }

        let phys = FOUR_GIB + (offset - low_ram_end);
        ram.read_into(phys, buf)
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
        // Restore ordering must be explicit and independent of snapshot file ordering so device
        // state is deterministic (especially for interrupt lines and PCI INTx routing).
        let mut interrupts_state = None;
        let mut pit_state = None;
        let mut rtc_state = None;
        let mut hpet_state = None;
        let mut acpi_pm_state = None;
        let mut pci_cfg_state = None;
        let mut pci_intx_state = None;
        let mut pci_legacy_state = None;

        for state in states {
            match state.id {
                // Prefer the dedicated `PLATFORM_INTERRUPTS` id, but accept the historical `APIC`
                // id for backward compatibility with older snapshots.
                DeviceId::PLATFORM_INTERRUPTS => interrupts_state = Some(state),
                DeviceId::APIC => {
                    if interrupts_state.is_none() {
                        interrupts_state = Some(state);
                    }
                }
                DeviceId::PIT => pit_state = Some(state),
                DeviceId::RTC => rtc_state = Some(state),
                DeviceId::HPET => hpet_state = Some(state),
                DeviceId::ACPI_PM => acpi_pm_state = Some(state),
                DeviceId::PCI_CFG => pci_cfg_state = Some(state),
                DeviceId::PCI_INTX_ROUTER => pci_intx_state = Some(state),
                // Backward compatibility: some snapshots stored PCI core state under the
                // historical `DeviceId::PCI`, either as a combined `PciCoreSnapshot` wrapper
                // (`PCIC`) or as a single `PCPT`/`INTX` payload.
                DeviceId::PCI => pci_legacy_state = Some(state),
                _ => {}
            }
        }

        // 1) Restore the platform interrupt sink first so it is valid for post-restore re-drive
        // steps.
        if let Some(state) = interrupts_state {
            if apply_io_snapshot_to_device(&state, &mut *self.pc.interrupts.borrow_mut()).is_err() {
                self.restore_error = Some(SnapshotError::Corrupt(
                    "failed to restore platform interrupts state",
                ));
                return;
            }
            self.restored_interrupts = true;
        }

        // 2) Restore core timer devices and ACPI PM.
        if let Some(state) = pit_state {
            if apply_io_snapshot_to_device(&state, &mut *self.pc.pit.borrow_mut()).is_err() {
                self.restore_error = Some(SnapshotError::Corrupt("failed to restore PIT state"));
                return;
            }
        }
        if let Some(state) = rtc_state {
            if apply_io_snapshot_to_device(&state, &mut *self.pc.rtc.borrow_mut()).is_err() {
                self.restore_error = Some(SnapshotError::Corrupt("failed to restore RTC state"));
                return;
            }
        }
        if let Some(state) = hpet_state {
            if apply_io_snapshot_to_device(&state, &mut *self.pc.hpet.borrow_mut()).is_err() {
                self.restore_error = Some(SnapshotError::Corrupt("failed to restore HPET state"));
                return;
            }
            self.restored_hpet = true;
        }
        if let Some(state) = acpi_pm_state {
            if apply_io_snapshot_to_device(&state, &mut *self.pc.acpi_pm.borrow_mut()).is_err() {
                self.restore_error =
                    Some(SnapshotError::Corrupt("failed to restore ACPI PM state"));
                return;
            }
        }

        // 3) Restore PCI core state.
        //
        // Prefer split canonical entries (`PCI_CFG` + `PCI_INTX_ROUTER`) when present, but accept
        // the historical `PCI` entry as a fallback for legacy snapshots.
        let has_pci_cfg_state = pci_cfg_state.is_some();
        let has_pci_intx_state = pci_intx_state.is_some();

        if let Some(state) = pci_cfg_state {
            if apply_io_snapshot_to_device(&state, &mut *self.pc.pci_cfg.borrow_mut()).is_err() {
                self.restore_error = Some(SnapshotError::Corrupt(
                    "failed to restore PCI config ports state",
                ));
                return;
            }
        }
        if let Some(state) = pci_intx_state {
            if apply_io_snapshot_to_device(&state, &mut self.pc.pci_intx).is_err() {
                self.restore_error =
                    Some(SnapshotError::Corrupt("failed to restore PCI INTx state"));
                return;
            }
            self.restored_pci_intx = true;
        }

        if !has_pci_cfg_state && !has_pci_intx_state {
            if let Some(state) = pci_legacy_state {
                let inner_id = state
                    .data
                    .get(8..12)
                    .and_then(|buf| <[u8; 4]>::try_from(buf).ok())
                    .unwrap_or([0u8; 4]);

                match inner_id {
                    // Combined wrapper: restore both config ports and router.
                    [b'P', b'C', b'I', b'C'] => {
                        let mut cfg_ports = self.pc.pci_cfg.borrow_mut();
                        let mut core = PciCoreSnapshot::new(&mut cfg_ports, &mut self.pc.pci_intx);
                        if apply_io_snapshot_to_device(&state, &mut core).is_err() {
                            self.restore_error =
                                Some(SnapshotError::Corrupt("failed to restore PCI core state"));
                            return;
                        }
                        self.restored_pci_intx = true;
                    }
                    // Legacy: config ports stored directly under `DeviceId::PCI`.
                    [b'P', b'C', b'P', b'T'] => {
                        if apply_io_snapshot_to_device(&state, &mut *self.pc.pci_cfg.borrow_mut())
                            .is_err()
                        {
                            self.restore_error = Some(SnapshotError::Corrupt(
                                "failed to restore legacy PCI config ports state",
                            ));
                        }
                    }
                    // Legacy: INTx router stored directly under `DeviceId::PCI`.
                    [b'I', b'N', b'T', b'X'] => {
                        if apply_io_snapshot_to_device(&state, &mut self.pc.pci_intx).is_err() {
                            self.restore_error = Some(SnapshotError::Corrupt(
                                "failed to restore legacy PCI INTx router state",
                            ));
                            return;
                        }
                        self.restored_pci_intx = true;
                    }
                    _ => {}
                }
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        usize::try_from(self.pc.ram_size_bytes).unwrap_or(0)
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        const FOUR_GIB: u64 = 0x1_0000_0000;
        let low_ram_end = crate::PCIE_ECAM_BASE;
        let ram_len = self.pc.ram_size_bytes;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(SnapshotError::Corrupt("ram write overflow"))?;
        if end > ram_len {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }

        // Important: bypass `MemoryBus::write_physical`, which applies A20 gating.
        let ram = self.pc.memory.ram_mut();
        if ram_len <= low_ram_end || data.is_empty() {
            ram.write_from(offset, data)
                .map_err(|_| SnapshotError::Corrupt("ram write failed"))?;
            return Ok(());
        }

        if offset < low_ram_end {
            let low_len = (low_ram_end - offset) as usize;
            let first = low_len.min(data.len());
            ram.write_from(offset, &data[..first])
                .map_err(|_| SnapshotError::Corrupt("ram write failed"))?;
            if first < data.len() {
                ram.write_from(FOUR_GIB, &data[first..])
                    .map_err(|_| SnapshotError::Corrupt("ram write failed"))?;
            }
            return Ok(());
        }

        let phys = FOUR_GIB + (offset - low_ram_end);
        ram.write_from(phys, data)
            .map_err(|_| SnapshotError::Corrupt("ram write failed"))?;
        Ok(())
    }

    fn post_restore(&mut self) -> Result<()> {
        if let Some(err) = self.restore_error.take() {
            return Err(err);
        }

        // `PciIntxRouter::load_state()` cannot drive the platform interrupt sink. Re-drive the
        // restored INTx levels into the sink after both sides have been restored.
        if self.restored_hpet || self.restored_pci_intx {
            if !self.restored_interrupts {
                return Err(SnapshotError::Corrupt(
                    "device state restored without platform interrupts state",
                ));
            }

            let mut interrupts = self.pc.interrupts.borrow_mut();

            // Same issue for PCI INTx: `PciIntxRouter::load_state()` restores internal bookkeeping
            // but cannot drive the sink.
            if self.restored_pci_intx {
                self.pc.pci_intx.sync_levels_to_sink(&mut *interrupts);
            }

            // `Hpet::load_state()` cannot drive its interrupt sink or restore its internal
            // `irq_asserted` handshake state; sync pending level-triggered lines into the platform
            // interrupt controller.
            //
            // This must run *after* PCI INTx sync: `PciIntxRouter::sync_levels_to_sink()` sets
            // routed GSI levels to either asserted or deasserted, and can otherwise clear a GSI
            // asserted by another device (e.g. HPET timer2 default route = GSI10, which is also
            // commonly used for PCI INTA#).
            if self.restored_hpet {
                self.pc
                    .hpet
                    .borrow_mut()
                    .sync_levels_to_sink(&mut *interrupts);
            }

            // Convert restored baseline GSI levels into ref-counted assertions once all devices
            // have re-driven their own interrupt outputs.
            interrupts.finalize_restore();
        } else if self.restored_interrupts {
            // Even if no post-restore device sync is required, clear any baseline line level state
            // so future callers can deassert lines normally.
            self.pc.interrupts.borrow_mut().finalize_restore();
        }

        Ok(())
    }
}
