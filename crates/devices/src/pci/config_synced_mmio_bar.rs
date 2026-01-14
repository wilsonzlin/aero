use super::{
    msi::PCI_CAP_ID_MSI, msix::PCI_CAP_ID_MSIX, MsiCapability, MsixCapability, PciBdf, PciDevice,
    SharedPciConfigPorts,
};
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

/// Generic adapter for PCI device models that maintain an internal PCI config image.
///
/// Many integrations keep a canonical PCI config space (typically [`SharedPciConfigPorts`]) that is
/// used for guest enumeration and for routing MMIO/PIO accesses to PCI BAR handlers. Some device
/// models also consult their own internal [`super::PciConfigSpace`] to gate MMIO behavior (e.g.
/// `COMMAND.MEM`) or to discover the BAR base being accessed.
///
/// When those two PCI config images diverge, device models can incorrectly treat MMIO as disabled
/// or use an incorrect BAR base. This wrapper keeps the device model's internal PCI config state
/// synchronized with the canonical config space on every MMIO access by mirroring:
/// - the PCI command register (offset `0x04`), and
/// - the base address of the accessed BAR.
///
/// In addition, this wrapper mirrors the guest-programmed MSI/MSI-X capability state (when
/// present) so device models that deliver interrupts from MMIO side effects observe the correct
/// interrupt configuration.
///
/// Note: MSI "pending bits" are device-managed (set when a vector is masked at the time an
/// interrupt is raised). In setups where the canonical PCI config space is decoupled from the
/// device model, the platform cannot observe device-generated pending bits, so this wrapper does
/// **not** overwrite the device model's pending bit latch when synchronizing MSI state.
///
/// This is intentionally BAR-scoped: it only syncs the BAR index passed at construction.
///
/// This replaces older one-off platform-specific wrappers (e.g. for AHCI/NVMe) with a reusable
/// generic adapter that can be applied to any `T: PciDevice + MmioHandler`.
pub struct PciConfigSyncedMmioBar<T> {
    pci_cfg: SharedPciConfigPorts,
    dev: Rc<RefCell<T>>,
    bdf: PciBdf,
    bar: u8,
}

impl<T> PciConfigSyncedMmioBar<T> {
    pub fn new(pci_cfg: SharedPciConfigPorts, dev: Rc<RefCell<T>>, bdf: PciBdf, bar: u8) -> Self {
        Self {
            pci_cfg,
            dev,
            bdf,
            bar,
        }
    }
}

impl<T: PciDevice> PciConfigSyncedMmioBar<T> {
    fn sync_pci_state(&mut self) {
        let (command, bar_base, msi_state, msix_state) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar_base = cfg
                .and_then(|cfg| cfg.bar_range(self.bar))
                .map(|range| range.base);
            let msi_state = cfg
                .and_then(|cfg| cfg.capability::<MsiCapability>())
                .map(|msi| {
                    (
                        msi.enabled(),
                        msi.message_address(),
                        msi.message_data(),
                        msi.mask_bits(),
                    )
                });
            let msix_state = cfg
                .and_then(|cfg| cfg.capability::<MsixCapability>())
                .map(|msix| (msix.enabled(), msix.function_masked()));
            (command, bar_base, msi_state, msix_state)
        };

        let mut dev = self.dev.borrow_mut();
        let cfg = dev.config_mut();
        cfg.set_command(command);
        if let Some(bar_base) = bar_base {
            cfg.set_bar_base(self.bar, bar_base);
        }

        // Keep MSI/MSI-X state synchronized as well. Some device models (e.g. xHCI) may
        // assert an interrupt condition as a side-effect of an MMIO write, and MSI delivery is
        // edge-triggered. If the device model observes the interrupt edge before it sees MSI
        // enabled, it can miss the MSI pulse and then suppress legacy INTx once MSI becomes active.
        //
        // Syncing the interrupt capability state here keeps "enable MSI, then touch MMIO" flows
        // deterministic and matches what real hardware does (the programmed MSI registers live in
        // PCI config space, not in the BAR window).
        if let Some((enabled, addr, data, mask)) = msi_state {
            if let Some(off) = cfg.find_capability(PCI_CAP_ID_MSI) {
                let base = u16::from(off);
                let ctrl = cfg.read(base + 0x02, 2) as u16;
                let is_64bit = (ctrl & (1 << 7)) != 0;
                let per_vector_masking = (ctrl & (1 << 8)) != 0;

                cfg.write(base + 0x04, 4, addr as u32);
                if is_64bit {
                    cfg.write(base + 0x08, 4, (addr >> 32) as u32);
                    cfg.write(base + 0x0c, 2, u32::from(data));
                    if per_vector_masking {
                        cfg.write(base + 0x10, 4, mask);
                    }
                } else {
                    cfg.write(base + 0x08, 2, u32::from(data));
                    if per_vector_masking {
                        cfg.write(base + 0x0c, 4, mask);
                    }
                }

                // Write Message Control last so the enabled bit is only observed after
                // address/data are synchronized.
                //
                // Only change the MSI Enable bit; preserve read-only capability bits (64-bit,
                // per-vector masking, etc.).
                let new_ctrl = if enabled {
                    ctrl | 0x0001
                } else {
                    ctrl & !0x0001
                };
                cfg.write(base + 0x02, 2, u32::from(new_ctrl));
            }
        }

        if let Some((enabled, function_masked)) = msix_state {
            if let Some(off) = cfg.find_capability(PCI_CAP_ID_MSIX) {
                let base = u16::from(off);
                let ctrl = cfg.read(base + 0x02, 2) as u16;
                let mut new_ctrl = ctrl;
                if enabled {
                    new_ctrl |= 1 << 15;
                } else {
                    new_ctrl &= !(1 << 15);
                }
                if function_masked {
                    new_ctrl |= 1 << 14;
                } else {
                    new_ctrl &= !(1 << 14);
                }
                cfg.write(base + 0x02, 2, u32::from(new_ctrl));
            }
        }
    }

    fn mirror_msi_pending_bits_to_platform_config(&mut self) {
        // MSI pending bits are device-managed: the device latches them when an interrupt is raised
        // while delivery is blocked (masked or unprogrammed address). If the platform maintains a
        // separate canonical PCI config space for guest reads, mirror the pending bits back so
        // config-space reads observe the device-managed state immediately.
        let pending_bits = self
            .dev
            .borrow()
            .config()
            .capability::<MsiCapability>()
            .map(|msi| msi.pending_bits())
            .unwrap_or(0);

        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let Some(cfg) = pci_cfg.bus_mut().device_config_mut(self.bdf) else {
            return;
        };
        if let Some(msi) = cfg.capability_mut::<MsiCapability>() {
            msi.set_pending_bits(pending_bits);
        }
    }
}

impl<T: MmioHandler + PciDevice> MmioHandler for PciConfigSyncedMmioBar<T> {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if !(1..=8).contains(&size) {
            return all_ones(size);
        }
        self.sync_pci_state();
        // Mask to avoid leaking junk in upper bits for sub-8-byte reads.
        let value = MmioHandler::read(&mut *self.dev.borrow_mut(), offset, size) & all_ones(size);
        self.mirror_msi_pending_bits_to_platform_config();
        value
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if !(1..=8).contains(&size) {
            return;
        }
        self.sync_pci_state();
        // Mask to enforce byte-enable semantics even for handlers that treat `value` as a full
        // 64-bit quantity.
        MmioHandler::write(
            &mut *self.dev.borrow_mut(),
            offset,
            size,
            value & all_ones(size),
        );
        self.mirror_msi_pending_bits_to_platform_config();
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
    use super::PciConfigSyncedMmioBar;
    use crate::pci::msi::PCI_CAP_ID_MSI;
    use crate::pci::msix::PCI_CAP_ID_MSIX;
    use crate::pci::{MsiCapability, MsixCapability};
    use crate::pci::{PciBarDefinition, PciBdf, PciConfigPorts, PciConfigSpace, PciDevice};
    use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};
    use memory::MmioHandler;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct TestPciConfigDevice {
        config: PciConfigSpace,
    }

    impl TestPciConfigDevice {
        fn new(bar: u8) -> Self {
            let mut config = PciConfigSpace::new(0x1234, 0x5678);
            config.set_bar_definition(
                bar,
                PciBarDefinition::Mmio32 {
                    size: 0x1000,
                    prefetchable: false,
                },
            );
            Self { config }
        }
    }

    impl PciDevice for TestPciConfigDevice {
        fn config(&self) -> &PciConfigSpace {
            &self.config
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.config
        }
    }

    struct TestMmioDevice {
        config: PciConfigSpace,
        bar: u8,
        last_command: u16,
        last_bar_base: u64,
    }

    impl TestMmioDevice {
        fn new(bar: u8) -> Self {
            let mut config = PciConfigSpace::new(0xabcd, 0xef01);
            config.set_bar_definition(
                bar,
                PciBarDefinition::Mmio32 {
                    size: 0x1000,
                    prefetchable: false,
                },
            );
            // Start with decoding disabled so we can observe the wrapper's sync behavior.
            config.set_command(0);
            Self {
                config,
                bar,
                last_command: 0,
                last_bar_base: 0,
            }
        }
    }

    impl PciDevice for TestMmioDevice {
        fn config(&self) -> &PciConfigSpace {
            &self.config
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.config
        }
    }

    impl MmioHandler for TestMmioDevice {
        fn read(&mut self, _offset: u64, _size: usize) -> u64 {
            self.last_command = self.config.command();
            self.last_bar_base = self
                .config
                .bar_range(self.bar)
                .map(|range| range.base)
                .unwrap_or(0);

            // Model a device that gates MMIO reads on COMMAND.MEM (bit 1), like AHCI.
            if (self.config.command() & 0x2) != 0 {
                0xA5A5_A5A5
            } else {
                0
            }
        }

        fn write(&mut self, _offset: u64, _size: usize, _value: u64) {
            self.last_command = self.config.command();
            self.last_bar_base = self
                .config
                .bar_range(self.bar)
                .map(|range| range.base)
                .unwrap_or(0);
        }
    }

    #[test]
    fn pci_config_synced_mmio_bar_syncs_command_and_bar_base_before_each_access() {
        let bdf = PciBdf::new(0, 2, 0);
        let bar = 0;

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(TestPciConfigDevice::new(bar)));

        let dev = Rc::new(RefCell::new(TestMmioDevice::new(bar)));
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev.clone(), bdf, bar);

        // Program canonical config space and ensure the wrapper mirrors it into the device model.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing config device");
            cfg.set_command(0x2);
            cfg.set_bar_base(bar, 0x1234_0000);
        }
        assert_eq!(mmio.read(0, 4), 0xA5A5_A5A5);
        {
            let dev = dev.borrow();
            assert_eq!(dev.config.command(), 0x2);
            assert_eq!(dev.config.bar_range(bar).unwrap().base, 0x1234_0000);
            assert_eq!(dev.last_command, 0x2);
            assert_eq!(dev.last_bar_base, 0x1234_0000);
        }

        // Update the canonical config again and ensure the wrapper re-syncs on the next access.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing config device");
            cfg.set_command(0);
            cfg.set_bar_base(bar, 0x5678_0000);
        }
        mmio.write(0x10, 4, 0);
        {
            let dev = dev.borrow();
            assert_eq!(dev.config.command(), 0);
            assert_eq!(dev.config.bar_range(bar).unwrap().base, 0x5678_0000);
            assert_eq!(dev.last_command, 0);
            assert_eq!(dev.last_bar_base, 0x5678_0000);
        }

        // BAR bases can be programmed back to zero by guests (e.g. device disable / teardown).
        // Ensure the wrapper mirrors a zero base too (not just "non-zero means programmed").
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing config device");
            cfg.set_bar_base(bar, 0);
        }
        mmio.read(0, 4);
        {
            let dev = dev.borrow();
            assert_eq!(dev.config.bar_range(bar).unwrap().base, 0);
            assert_eq!(dev.last_bar_base, 0);
        }
    }

    #[test]
    fn pci_config_synced_mmio_bar_syncs_msi_capability_state() {
        let bdf = PciBdf::new(0, 4, 0);
        let bar = 0;

        struct TestPciConfigDeviceMsi {
            config: PciConfigSpace,
        }

        impl TestPciConfigDeviceMsi {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0x1234, 0x5678);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsiCapability::new()));
                Self { config }
            }
        }

        impl PciDevice for TestPciConfigDeviceMsi {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        struct TestMmioDeviceMsi {
            config: PciConfigSpace,
            msi_enabled: bool,
            msi_addr: u64,
            msi_data: u16,
        }

        impl TestMmioDeviceMsi {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0xabcd, 0xef01);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                // Add an unrelated capability first so the MSI capability is not located at the
                // canonical 0x40 offset. This ensures the wrapper uses capability lookup rather
                // than assuming matching offsets between the canonical config and the device model
                // config spaces.
                config.add_capability(Box::new(MsixCapability::new(1, 0, 0, 0, 0x1000)));
                config.add_capability(Box::new(MsiCapability::new()));
                Self {
                    config,
                    msi_enabled: false,
                    msi_addr: 0,
                    msi_data: 0,
                }
            }
        }

        impl PciDevice for TestMmioDeviceMsi {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        impl MmioHandler for TestMmioDeviceMsi {
            fn read(&mut self, _offset: u64, _size: usize) -> u64 {
                let cap = self
                    .config
                    .capability::<MsiCapability>()
                    .expect("missing MSI capability");
                self.msi_enabled = cap.enabled();
                self.msi_addr = cap.message_address();
                self.msi_data = cap.message_data();
                0
            }

            fn write(&mut self, _offset: u64, _size: usize, _value: u64) {
                let cap = self
                    .config
                    .capability::<MsiCapability>()
                    .expect("missing MSI capability");
                self.msi_enabled = cap.enabled();
                self.msi_addr = cap.message_address();
                self.msi_data = cap.message_data();
            }
        }

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(TestPciConfigDeviceMsi::new(bar)));

        let dev = Rc::new(RefCell::new(TestMmioDeviceMsi::new(bar)));
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev.clone(), bdf, bar);

        // Program MSI in the canonical config space.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing config device");
            let cap_off = cfg
                .find_capability(PCI_CAP_ID_MSI)
                .expect("canonical config missing MSI capability") as u16;

            cfg.write(cap_off + 0x04, 4, 0xfee0_0000);
            cfg.write(cap_off + 0x08, 4, 0);
            cfg.write(cap_off + 0x0c, 2, 0x0045);
            let ctrl = cfg.read(cap_off + 0x02, 2) as u16;
            cfg.write(cap_off + 0x02, 2, u32::from(ctrl | 0x0001));
        }

        // Trigger an MMIO read so the wrapper synchronizes PCI state into the device model.
        mmio.read(0, 4);

        let dev = dev.borrow();
        assert!(
            dev.msi_enabled,
            "device model should observe MSI enable from platform config"
        );
        assert_eq!(dev.msi_addr, 0xfee0_0000);
        assert_eq!(dev.msi_data, 0x0045);
    }

    #[test]
    fn pci_config_synced_mmio_bar_preserves_device_managed_msi_pending_bits() {
        let bdf = PciBdf::new(0, 7, 0);
        let bar = 0;

        struct CanonicalCfgDev {
            config: PciConfigSpace,
        }

        impl CanonicalCfgDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0x1234, 0x5678);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsiCapability::new()));
                Self { config }
            }
        }

        impl PciDevice for CanonicalCfgDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        struct MmioDev {
            config: PciConfigSpace,
        }

        impl MmioDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0xabcd, 0xef01);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsiCapability::new()));
                Self { config }
            }
        }

        impl PciDevice for MmioDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        impl MmioHandler for MmioDev {
            fn read(&mut self, _offset: u64, _size: usize) -> u64 {
                0
            }

            fn write(&mut self, _offset: u64, _size: usize, _value: u64) {}
        }

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(CanonicalCfgDev::new(bar)));

        // Program MSI in the canonical config space (unmasked).
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing canonical config device");
            cfg.set_command(0x2);
            cfg.set_bar_base(bar, 0x1234_0000);

            let cap_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
            cfg.write(cap_off + 0x04, 4, 0xfee0_0000);
            cfg.write(cap_off + 0x08, 4, 0);
            cfg.write(cap_off + 0x0c, 2, 0x0045);
            let ctrl = cfg.read(cap_off + 0x02, 2) as u16;
            cfg.write(cap_off + 0x02, 2, u32::from(ctrl | 0x0001));
        }

        let dev = Rc::new(RefCell::new(MmioDev::new(bar)));

        // Latch a pending bit in the MMIO device model's MSI capability without synchronizing it
        // into the raw config bytes (simulates a device-generated pending interrupt).
        {
            let mut dev = dev.borrow_mut();
            let cfg = dev.config_mut();
            let cap_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
            cfg.write(cap_off + 0x04, 4, 0xfee0_0000);
            cfg.write(cap_off + 0x08, 4, 0);
            cfg.write(cap_off + 0x0c, 2, 0x0045);
            let ctrl = cfg.read(cap_off + 0x02, 2) as u16;
            cfg.write(cap_off + 0x02, 2, u32::from(ctrl | 0x0001));

            let is_64bit = (ctrl & (1 << 7)) != 0;
            let per_vector_masking = (ctrl & (1 << 8)) != 0;
            assert!(
                per_vector_masking,
                "test requires per-vector masking support"
            );
            let mask_off = if is_64bit {
                cap_off + 0x10
            } else {
                cap_off + 0x0c
            };
            cfg.write(mask_off, 4, 1);

            struct Sink;
            impl MsiTrigger for Sink {
                fn trigger_msi(&mut self, _message: MsiMessage) {}
            }

            let msi = cfg.capability_mut::<MsiCapability>().unwrap();
            assert!(!msi.trigger(&mut Sink));
            assert_eq!(msi.pending_bits() & 1, 1);
        }

        // Trigger sync: canonical config should unmask MSI, but must not clobber the device-managed
        // pending bit.
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev.clone(), bdf, bar);
        mmio.read(0, 4);

        let dev = dev.borrow();
        let msi = dev.config.capability::<MsiCapability>().unwrap();
        assert_eq!(
            msi.mask_bits() & 1,
            0,
            "mask bits should sync from canonical config"
        );
        assert_eq!(
            msi.pending_bits() & 1,
            1,
            "device-managed pending bit must not be overwritten by config synchronization"
        );
    }

    #[test]
    fn pci_config_synced_mmio_bar_mirrors_device_managed_msi_pending_bits_to_canonical_config() {
        let bdf = PciBdf::new(0, 8, 0);
        let bar = 0;

        struct CanonicalCfgDev {
            config: PciConfigSpace,
        }

        impl CanonicalCfgDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0x1234, 0x5678);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsiCapability::new()));
                Self { config }
            }
        }

        impl PciDevice for CanonicalCfgDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        struct MmioDev {
            config: PciConfigSpace,
        }

        impl MmioDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0xabcd, 0xef01);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsiCapability::new()));
                Self { config }
            }
        }

        impl PciDevice for MmioDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        impl MmioHandler for MmioDev {
            fn read(&mut self, _offset: u64, _size: usize) -> u64 {
                0
            }

            fn write(&mut self, _offset: u64, _size: usize, _value: u64) {
                struct Sink;
                impl MsiTrigger for Sink {
                    fn trigger_msi(&mut self, _message: MsiMessage) {}
                }

                let Some(msi) = self.config.capability_mut::<MsiCapability>() else {
                    panic!("missing MSI capability");
                };
                let _ = msi.trigger(&mut Sink);
            }
        }

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(CanonicalCfgDev::new(bar)));

        // Enable MSI in canonical config with an invalid message address (addr=0) so triggering
        // latches the pending bit.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing canonical config device");
            cfg.set_command(0x2);
            cfg.set_bar_base(bar, 0x1234_0000);

            let cap_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
            cfg.write(cap_off + 0x04, 4, 0);
            cfg.write(cap_off + 0x08, 4, 0);
            cfg.write(cap_off + 0x0c, 2, 0x0045);
            cfg.write(cap_off + 0x10, 4, 0); // unmasked
            let ctrl = cfg.read(cap_off + 0x02, 2) as u16;
            cfg.write(cap_off + 0x02, 2, u32::from(ctrl | 0x0001));
        }

        let dev = Rc::new(RefCell::new(MmioDev::new(bar)));
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev, bdf, bar);

        // Trigger a device interrupt attempt via MMIO. It should latch the pending bit and the
        // wrapper should mirror it back into canonical config space.
        mmio.write(0, 4, 0);

        let pending_bits = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .unwrap()
                .capability::<MsiCapability>()
                .unwrap()
                .pending_bits()
        };
        assert_eq!(pending_bits & 1, 1);

        // Now program a valid MSI address in canonical config and trigger again; delivery should
        // clear the pending bit, and the wrapper should mirror that clear back.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config_mut(bdf).unwrap();
            let cap_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
            cfg.write(cap_off + 0x04, 4, 0xfee0_0000);
            cfg.write(cap_off + 0x08, 4, 0);
        }
        mmio.write(0, 4, 0);

        let pending_bits = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .unwrap()
                .capability::<MsiCapability>()
                .unwrap()
                .pending_bits()
        };
        assert_eq!(pending_bits & 1, 0);
    }

    #[test]
    fn pci_config_synced_mmio_bar_syncs_msi_state_before_mmio_access() {
        let bdf = PciBdf::new(0, 6, 0);
        let bar = 0;

        struct CanonicalCfgDev {
            config: PciConfigSpace,
        }

        impl CanonicalCfgDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0x1234, 0x5678);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                // Use a 32-bit MSI capability with no per-vector mask/pending bits to ensure the
                // wrapper does not assume the 64-bit capability layout.
                config.add_capability(Box::new(MsiCapability::new_with_config(false, false)));
                Self { config }
            }
        }

        impl PciDevice for CanonicalCfgDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        struct MmioDev {
            config: PciConfigSpace,
        }

        impl MmioDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0xabcd, 0xef01);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsiCapability::new_with_config(false, false)));
                // Start with decoding disabled so we can observe the wrapper's sync behavior.
                config.set_command(0);
                Self { config }
            }
        }

        impl PciDevice for MmioDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        impl MmioHandler for MmioDev {
            fn read(&mut self, _offset: u64, _size: usize) -> u64 {
                0
            }

            fn write(&mut self, _offset: u64, _size: usize, _value: u64) {}
        }

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(CanonicalCfgDev::new(bar)));

        // Program MSI in the canonical config space.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing canonical config device");
            cfg.set_command(0x2);
            cfg.set_bar_base(bar, 0x1234_0000);

            let cap = cfg
                .find_capability(PCI_CAP_ID_MSI)
                .expect("canonical config should contain MSI") as u16;
            cfg.write(cap + 0x04, 4, 0xfee0_0000);
            cfg.write(cap + 0x08, 2, 0x0045);
            let ctrl = cfg.read(cap + 0x02, 2) as u16;
            cfg.write(cap + 0x02, 2, u32::from(ctrl | 0x0001));
        }

        let dev = Rc::new(RefCell::new(MmioDev::new(bar)));
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev.clone(), bdf, bar);

        // Trigger synchronization.
        mmio.read(0, 4);

        // MSI state should now be visible in the MMIO device model's config space.
        let mut dev = dev.borrow_mut();
        let msi = dev
            .config_mut()
            .capability::<MsiCapability>()
            .expect("device model should have MSI capability");
        assert!(msi.enabled());
        assert_eq!(msi.message_address(), 0xfee0_0000);
        assert_eq!(msi.message_data(), 0x0045);
    }

    #[test]
    fn pci_config_synced_mmio_bar_syncs_msix_enable_bits_before_mmio_access() {
        let bdf = PciBdf::new(0, 5, 0);
        let bar = 0;

        struct CanonicalCfgDev {
            config: PciConfigSpace,
        }

        impl CanonicalCfgDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0x1234, 0x5678);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x4000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsixCapability::new(1, 0, 0, 0, 0x1000)));
                Self { config }
            }
        }

        impl PciDevice for CanonicalCfgDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        struct MmioDev {
            config: PciConfigSpace,
        }

        impl MmioDev {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0xabcd, 0xef01);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x4000,
                        prefetchable: false,
                    },
                );
                config.add_capability(Box::new(MsixCapability::new(1, 0, 0, 0, 0x1000)));
                config.set_command(0);
                Self { config }
            }
        }

        impl PciDevice for MmioDev {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        impl MmioHandler for MmioDev {
            fn read(&mut self, _offset: u64, _size: usize) -> u64 {
                0
            }

            fn write(&mut self, _offset: u64, _size: usize, _value: u64) {}
        }

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(CanonicalCfgDev::new(bar)));

        // Enable MSI-X in the canonical config space (also set Function Mask to ensure it syncs).
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing canonical config device");
            cfg.set_command(0x2);
            cfg.set_bar_base(bar, 0x1234_0000);

            let cap = cfg
                .find_capability(PCI_CAP_ID_MSIX)
                .expect("canonical config should contain MSI-X") as u16;
            let ctrl = cfg.read(cap + 0x02, 2) as u16;
            cfg.write(cap + 0x02, 2, u32::from(ctrl | (1 << 15) | (1 << 14)));
        }

        let dev = Rc::new(RefCell::new(MmioDev::new(bar)));
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev.clone(), bdf, bar);
        mmio.read(0, 4);

        let mut dev = dev.borrow_mut();
        let msix = dev
            .config_mut()
            .capability::<MsixCapability>()
            .expect("device model should have MSI-X capability");
        assert!(msix.enabled());
        assert!(msix.function_masked());
    }

    #[test]
    fn pci_config_synced_mmio_bar_masks_values_to_access_size() {
        let bdf = PciBdf::new(0, 3, 0);
        let bar = 0;

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(TestPciConfigDevice::new(bar)));

        struct UnmaskedMmioDevice {
            config: PciConfigSpace,
            last_write_value: u64,
        }

        impl UnmaskedMmioDevice {
            fn new(bar: u8) -> Self {
                let mut config = PciConfigSpace::new(0xabcd, 0xef01);
                config.set_bar_definition(
                    bar,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                Self {
                    config,
                    last_write_value: 0,
                }
            }
        }

        impl PciDevice for UnmaskedMmioDevice {
            fn config(&self) -> &PciConfigSpace {
                &self.config
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.config
            }
        }

        impl MmioHandler for UnmaskedMmioDevice {
            fn read(&mut self, _offset: u64, _size: usize) -> u64 {
                u64::MAX
            }

            fn write(&mut self, _offset: u64, _size: usize, value: u64) {
                self.last_write_value = value;
            }
        }

        // Enable MEM decoding so the wrapper syncs a non-zero command register.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing config device");
            cfg.set_command(0x2);
            cfg.set_bar_base(bar, 0x1234_0000);
        }

        let dev = Rc::new(RefCell::new(UnmaskedMmioDevice::new(bar)));
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev.clone(), bdf, bar);

        // Underlying device returns u64::MAX regardless of size, but the wrapper should mask it.
        assert_eq!(mmio.read(0, 1), 0xFF);
        assert_eq!(mmio.read(0, 4), 0xFFFF_FFFF);

        // Writes should also be masked before reaching the device.
        mmio.write(0, 4, u64::MAX);
        assert_eq!(dev.borrow().last_write_value, 0xFFFF_FFFF);
    }
}
