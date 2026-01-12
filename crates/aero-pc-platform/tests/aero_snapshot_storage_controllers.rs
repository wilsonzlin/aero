use std::io::Cursor;

use aero_devices::pci::profile;
use aero_devices_storage::ata::ATA_CMD_READ_DMA_EXT;
use aero_devices_storage::pci_ide::PRIMARY_PORTS;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_io_snapshot::io::storage::state::DiskControllersSnapshot;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result as SnapshotResult, SaveOptions, SnapshotMeta, SnapshotSource, SnapshotTarget,
};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use memory::MemoryBus as _;

mod helpers;
use helpers::*;

const RAM_SIZE: usize = 2 * 1024 * 1024;

struct PcPlatformStorageSnapshotHarness {
    platform: PcPlatform,
    meta: SnapshotMeta,
}

impl PcPlatformStorageSnapshotHarness {
    fn new(ram_size: usize) -> Self {
        Self::new_with_config(ram_size, PcPlatformConfig::default())
    }

    fn new_with_config(ram_size: usize, config: PcPlatformConfig) -> Self {
        Self {
            platform: PcPlatform::new_with_config(ram_size, config),
            meta: SnapshotMeta {
                snapshot_id: 1,
                parent_snapshot_id: None,
                created_unix_ms: 0,
                label: None,
            },
        }
    }
}

impl SnapshotSource for PcPlatformStorageSnapshotHarness {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        let mut devices = Vec::new();
        // PCI config mechanism #1 ports + PCI bus state (BARs/command bits).
        devices.push(device_state_from_io_snapshot(
            DeviceId::PCI_CFG,
            &*self.platform.pci_cfg.borrow(),
        ));

        // Storage controller(s).
        //
        // Canonical encoding for the outer `DeviceId::DISK_CONTROLLER` entry is the `DSKC` wrapper
        // (`DiskControllersSnapshot`). This allows a single device entry to carry multiple
        // different controller snapshots (AHCI + IDE + NVMe + virtio-blk) keyed by PCI BDF.
        let mut disk_controllers = DiskControllersSnapshot::new();

        // For this harness we only snapshot the ICH9 AHCI controller at its canonical BDF.
        if let Some(ahci) = &self.platform.ahci {
            disk_controllers.insert(
                profile::SATA_AHCI_ICH9.bdf.pack_u16(),
                ahci.borrow().save_state(),
            );
        }
        if let Some(ide) = &self.platform.ide {
            disk_controllers.insert(profile::IDE_PIIX3.bdf.pack_u16(), ide.borrow().save_state());
        }
        devices.push(device_state_from_io_snapshot(
            DeviceId::DISK_CONTROLLER,
            &disk_controllers,
        ));

        devices
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        usize::try_from(self.platform.memory.ram().size()).unwrap_or(0)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> SnapshotResult<()> {
        // Snapshot RAM reads must bypass A20 gating (same rationale as other platform snapshot tests).
        self.platform
            .memory
            .ram()
            .read_into(offset, buf)
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram read out of range"))?;
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

impl SnapshotTarget for PcPlatformStorageSnapshotHarness {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        // Restore ordering is explicit so controller restore sees the correct PCI bus state.
        let mut pci_cfg = None;
        let mut disk_controllers = None;

        for state in states {
            match state.id {
                DeviceId::PCI_CFG => pci_cfg = Some(state),
                DeviceId::DISK_CONTROLLER => disk_controllers = Some(state),
                _ => {}
            }
        }

        if let Some(state) = pci_cfg {
            apply_io_snapshot_to_device(&state, &mut *self.platform.pci_cfg.borrow_mut()).unwrap();
        }

        if let Some(state) = disk_controllers {
            // `DeviceId::DISK_CONTROLLER` may contain multiple controller types/versions.
            // Route based on the inner `aero-io-snapshot` device id.
            let id = state
                .data
                .get(8..12)
                .unwrap_or(&[])
                .try_into()
                .unwrap_or([0u8; 4]);
            match id {
                // Canonical `DSKC` wrapper.
                [b'D', b'S', b'K', b'C'] => {
                    let mut wrapper = DiskControllersSnapshot::default();
                    apply_io_snapshot_to_device(&state, &mut wrapper).unwrap();

                    // Apply only controller entries that exist in the target machine.
                    for (&packed_bdf, nested) in wrapper.controllers() {
                        let bdf = aero_devices::pci::PciBdf::unpack_u16(packed_bdf);
                        if bdf == profile::SATA_AHCI_ICH9.bdf {
                            if let Some(ahci) = &self.platform.ahci {
                                ahci.borrow_mut().load_state(nested).unwrap();
                            }
                        }
                        if bdf == profile::IDE_PIIX3.bdf {
                            if let Some(ide) = &self.platform.ide {
                                ide.borrow_mut().load_state(nested).unwrap();
                            }
                        }
                    }
                }
                // Backward compatibility: some snapshots stored the controller directly under
                // `DeviceId::DISK_CONTROLLER` without the `DSKC` wrapper.
                [b'A', b'H', b'C', b'P'] => {
                    let ahci = self.platform.ahci.as_ref().expect("AHCI enabled");
                    apply_io_snapshot_to_device(&state, &mut *ahci.borrow_mut()).unwrap();
                }
                [b'I', b'D', b'E', b'0'] => {
                    let ide = self.platform.ide.as_ref().expect("IDE enabled");
                    apply_io_snapshot_to_device(&state, &mut *ide.borrow_mut()).unwrap();
                }
                _ => panic!("unexpected DISK_CONTROLLER payload device id: {id:?}"),
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        usize::try_from(self.platform.memory.ram().size()).unwrap_or(0)
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> SnapshotResult<()> {
        self.platform
            .memory
            .ram_mut()
            .write_from(offset, data)
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram write out of range"))?;
        Ok(())
    }
}

fn save_snapshot_bytes(source: &mut PcPlatformStorageSnapshotHarness) -> Vec<u8> {
    let mut options = SaveOptions::default();
    options.ram.compression = Compression::None;
    options.ram.chunk_size = 4096;

    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, source, options).unwrap();
    cursor.into_inner()
}

#[test]
fn aero_snapshot_roundtrip_preserves_ahci_inflight_dma_command_and_allows_resume() {
    let mut src = PcPlatformStorageSnapshotHarness::new(RAM_SIZE);

    // Attach a small in-memory disk with a known marker at LBA 4.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(4 * SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    src.platform.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    let bdf = profile::SATA_AHCI_ICH9.bdf;

    // Enable MMIO decoding + bus mastering so the guest can program the ABAR and the device can DMA.
    pci_enable_mmio(&mut src.platform, bdf);
    pci_enable_bus_mastering(&mut src.platform, bdf);

    let bar5 = pci_read_bar(&mut src.platform, bdf, profile::AHCI_ABAR_BAR_INDEX);
    assert_eq!(bar5.kind, BarKind::Mem32);
    assert_ne!(bar5.base, 0);

    // Guest memory layout for command list + table + DMA buffer.
    let mut alloc = GuestAllocator::new(RAM_SIZE as u64, 0x1000);
    let clb = alloc.alloc_bytes(1024, 1024);
    let fb = alloc.alloc_bytes(256, 256);
    let ctba = alloc.alloc_bytes(256, 128);
    let data_buf = alloc.alloc_bytes(SECTOR_SIZE, 512);

    // Program the AHCI registers (port 0).
    const HBA_GHC: u64 = 0x04;
    const PORT_BASE: u64 = 0x100;
    const PORT_CLB: u64 = 0x00;
    const PORT_CLBU: u64 = 0x04;
    const PORT_FB: u64 = 0x08;
    const PORT_FBU: u64 = 0x0C;
    const PORT_IS: u64 = 0x10;
    const PORT_IE: u64 = 0x14;
    const PORT_CMD: u64 = 0x18;
    const PORT_CI: u64 = 0x38;

    const GHC_IE: u32 = 1 << 1;
    const GHC_AE: u32 = 1 << 31;
    const PORT_IS_DHRS: u32 = 1 << 0;
    const PORT_CMD_ST: u32 = 1 << 0;
    const PORT_CMD_FRE: u32 = 1 << 4;

    src.platform
        .memory
        .write_u32(bar5.base + HBA_GHC, GHC_IE | GHC_AE);
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_CLB, clb as u32);
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_CLBU, 0);
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_FB, fb as u32);
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_FBU, 0);
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_IE, PORT_IS_DHRS);
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_CMD, PORT_CMD_ST | PORT_CMD_FRE);

    // Build a single-slot command list entry: READ DMA EXT (LBA=4, 1 sector).
    let cfl = 5u32; // 20 bytes / 4
    let prdtl = 1u32;
    let header_flags = cfl | (prdtl << 16);
    src.platform.memory.write_u32(clb, header_flags);
    src.platform.memory.write_u32(clb + 4, 0); // PRDBC
    src.platform.memory.write_u32(clb + 8, ctba as u32);
    src.platform.memory.write_u32(clb + 12, 0);

    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = ATA_CMD_READ_DMA_EXT;
    cfis[7] = 0x40; // LBA mode
    let lba: u64 = 4;
    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;
    cfis[12] = 1;
    mem_write(&mut src.platform, ctba, &cfis);

    // PRDT entry 0.
    let prd = ctba + 0x80;
    src.platform.memory.write_u32(prd, data_buf as u32);
    src.platform.memory.write_u32(prd + 4, 0);
    src.platform.memory.write_u32(prd + 8, 0);
    src.platform
        .memory
        .write_u32(prd + 12, (SECTOR_SIZE as u32 - 1) | (1 << 31));

    // Clear any prior interrupt state and issue the command.
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_IS, PORT_IS_DHRS);
    src.platform
        .memory
        .write_u32(bar5.base + PORT_BASE + PORT_CI, 1);

    // Capture the expected device snapshot bytes before saving the VM snapshot.
    let expected_pci_cfg = src.platform.pci_cfg.borrow().save_state();
    let expected_ahci = src
        .platform
        .ahci
        .as_ref()
        .expect("AHCI enabled")
        .borrow()
        .save_state();

    let snap = save_snapshot_bytes(&mut src);

    let mut restored = PcPlatformStorageSnapshotHarness::new(RAM_SIZE);
    restore_snapshot(&mut Cursor::new(&snap), &mut restored).unwrap();

    // Device snapshot blobs should roundtrip byte-identically through the container.
    assert_eq!(
        restored.platform.pci_cfg.borrow().save_state(),
        expected_pci_cfg
    );
    assert_eq!(
        restored
            .platform
            .ahci
            .as_ref()
            .expect("AHCI enabled")
            .borrow()
            .save_state(),
        expected_ahci
    );

    // Host contract: storage controller restore drops attached disks; reattach after restoring state.
    let mut disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    disk.write_at(4 * SECTOR_SIZE as u64, &[9, 8, 7, 6])
        .unwrap();
    restored
        .platform
        .attach_ahci_disk_port0(Box::new(disk))
        .unwrap();

    // Resume device processing and verify the DMA completes.
    restored.platform.process_ahci();

    let mut out = [0u8; 4];
    mem_read(&mut restored.platform, data_buf, &mut out);
    assert_eq!(out, [9, 8, 7, 6]);

    // Verify the AHCI interrupt is asserted and is routed through PCI INTx -> PIC.
    let expected_irq = {
        let pin = profile::SATA_AHCI_ICH9
            .interrupt_pin
            .expect("profile should provide interrupt pin");
        u8::try_from(
            restored
                .platform
                .pci_intx
                .gsi_for_intx(profile::SATA_AHCI_ICH9.bdf, pin),
        )
        .unwrap()
    };
    unmask_pic_irq(&mut restored.platform, expected_irq);
    assert!(
        restored
            .platform
            .ahci
            .as_ref()
            .unwrap()
            .borrow()
            .intx_level(),
        "AHCI INTx should be asserted after completing the DMA command"
    );
    restored.platform.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&restored.platform), Some(expected_irq));
}

#[test]
fn aero_snapshot_roundtrip_preserves_ide_inflight_pio_and_allows_resume() {
    let mut src = PcPlatformStorageSnapshotHarness::new_with_config(
        RAM_SIZE,
        PcPlatformConfig {
            enable_ahci: false,
            enable_ide: true,
            enable_uhci: false,
            ..Default::default()
        },
    );

    // Attach a small in-memory disk with a known marker at LBA 0.
    let mut disk = RawDisk::create(MemBackend::new(), 4 * SECTOR_SIZE as u64).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_at(0, &sector0).unwrap();
    src.platform
        .attach_ide_primary_master_disk(Box::new(disk))
        .unwrap();

    // Issue READ SECTORS (LBA 0, 1 sector) and consume the first 4 bytes ("BOOT").
    src.platform.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    src.platform.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    src.platform.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    src.platform.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    src.platform.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    src.platform.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20);

    let w0 = src.platform.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let w1 = src.platform.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let mut first4 = [0u8; 4];
    first4[0..2].copy_from_slice(&w0.to_le_bytes());
    first4[2..4].copy_from_slice(&w1.to_le_bytes());
    assert_eq!(&first4, b"BOOT");

    let snap = save_snapshot_bytes(&mut src);

    let mut restored = PcPlatformStorageSnapshotHarness::new_with_config(
        RAM_SIZE,
        PcPlatformConfig {
            enable_ahci: false,
            enable_ide: true,
            enable_uhci: false,
            ..Default::default()
        },
    );
    restore_snapshot(&mut Cursor::new(&snap), &mut restored).unwrap();

    // IRQ should still be pending (PIO data phase).
    assert!(
        restored
            .platform
            .ide
            .as_ref()
            .unwrap()
            .borrow()
            .controller
            .primary_irq_pending(),
        "restored IDE controller should preserve pending IRQ during PIO data phase"
    );

    // Ensure the platform observes the IDE IRQ14 assertion when polled.
    unmask_pic_irq(&mut restored.platform, 14);
    restored.platform.poll_pci_intx_lines();
    assert_eq!(pic_pending_irq(&restored.platform), Some(14));

    // Read the rest of the sector and ensure it's still correct.
    let mut buf = vec![0u8; SECTOR_SIZE];
    buf[0..4].copy_from_slice(b"BOOT");
    for i in 2..(SECTOR_SIZE / 2) {
        let w = restored.platform.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&buf[0..4], b"BOOT");

    // Reading status clears the pending IRQ.
    let _ = restored.platform.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!restored
        .platform
        .ide
        .as_ref()
        .unwrap()
        .borrow()
        .controller
        .primary_irq_pending());

    // Re-attach a backend and perform a WRITE SECTORS PIO to LBA 1, then read it back.
    let disk2 = RawDisk::create(MemBackend::new(), 4 * SECTOR_SIZE as u64).unwrap();
    restored
        .platform
        .attach_ide_primary_master_disk(Box::new(disk2))
        .unwrap();

    restored
        .platform
        .io
        .write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    restored
        .platform
        .io
        .write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30); // WRITE SECTORS

    restored
        .platform
        .io
        .write(PRIMARY_PORTS.cmd_base, 2, u16::from_le_bytes([5, 6]) as u32);
    restored
        .platform
        .io
        .write(PRIMARY_PORTS.cmd_base, 2, u16::from_le_bytes([7, 8]) as u32);
    for _ in 0..((SECTOR_SIZE / 2) - 2) {
        restored.platform.io.write(PRIMARY_PORTS.cmd_base, 2, 0);
    }

    // Read back LBA 1 and verify the first 4 bytes.
    restored
        .platform
        .io
        .write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    restored.platform.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    restored
        .platform
        .io
        .write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20); // READ SECTORS

    let w0 = restored.platform.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let w1 = restored.platform.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let mut out = [0u8; 4];
    out[0..2].copy_from_slice(&w0.to_le_bytes());
    out[2..4].copy_from_slice(&w1.to_le_bytes());
    assert_eq!(out, [5, 6, 7, 8]);
}
