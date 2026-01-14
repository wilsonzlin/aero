#![cfg(not(target_arch = "wasm32"))]

use aero_io_snapshot::io::state::IoSnapshot;
use emulator::io::storage::ahci::{registers, AhciController};
use emulator::io::storage::disk::{DiskBackend, DiskResult, MemDisk};
use memory::MemoryBus;
use std::sync::{Arc, Mutex};

const ATA_CMD_READ_DMA_EXT: u8 = 0x25;

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large for VecMemory");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let range = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[range]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let range = self.range(paddr, buf.len());
        self.data[range].copy_from_slice(buf);
    }
}

#[derive(Clone)]
struct SharedDisk(Arc<Mutex<MemDisk>>);

impl DiskBackend for SharedDisk {
    fn sector_size(&self) -> u32 {
        self.0.lock().unwrap().sector_size()
    }

    fn total_sectors(&self) -> u64 {
        self.0.lock().unwrap().total_sectors()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.0.lock().unwrap().read_sectors(lba, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.0.lock().unwrap().write_sectors(lba, buf)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.0.lock().unwrap().flush()
    }
}

fn build_cmd_header(cfl_dwords: u32, write: bool, prdt_len: u16, ctba: u64) -> [u8; 32] {
    let mut buf = [0u8; 32];
    let mut dw0 = cfl_dwords & 0x1f;
    if write {
        dw0 |= 1 << 6;
    }
    dw0 |= (u32::from(prdt_len)) << 16;
    buf[0..4].copy_from_slice(&dw0.to_le_bytes());
    buf[8..12].copy_from_slice(&(ctba as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&((ctba >> 32) as u32).to_le_bytes());
    buf
}

fn write_reg_h2d_fis(mem: &mut VecMemory, addr: u64, cmd: u8, lba: u64, count: u16) {
    let mut fis = [0u8; 64];
    fis[0] = 0x27; // Register H2D FIS
    fis[1] = 0x80; // C=1
    fis[2] = cmd;

    fis[4] = (lba & 0xff) as u8;
    fis[5] = ((lba >> 8) & 0xff) as u8;
    fis[6] = ((lba >> 16) & 0xff) as u8;
    fis[7] = 0x40; // device: LBA mode
    fis[8] = ((lba >> 24) & 0xff) as u8;
    fis[9] = ((lba >> 32) & 0xff) as u8;
    fis[10] = ((lba >> 40) & 0xff) as u8;

    fis[12] = (count & 0xff) as u8;
    fis[13] = (count >> 8) as u8;

    mem.write_physical(addr, &fis);
}

fn write_prd(mem: &mut VecMemory, addr: u64, dba: u64, len: u32) {
    let mut prd = [0u8; 16];
    prd[0..4].copy_from_slice(&(dba as u32).to_le_bytes());
    prd[4..8].copy_from_slice(&((dba >> 32) as u32).to_le_bytes());
    let dbc = len.saturating_sub(1) & 0x003f_ffff;
    prd[12..16].copy_from_slice(&dbc.to_le_bytes());
    mem.write_physical(addr, &prd);
}

#[test]
fn snapshot_roundtrip_preserves_mmio_state_and_requires_disk_reattach_for_dma() {
    let disk = Arc::new(Mutex::new(MemDisk::new(16)));
    {
        let mut d = disk.lock().unwrap();
        for (i, b) in d.data_mut().iter_mut().enumerate() {
            *b = (i & 0xff) as u8;
        }
    }
    let shared_disk = SharedDisk(disk.clone());

    let mut mem = VecMemory::new(0x20_000);
    let mut controller = AhciController::new(Box::new(shared_disk.clone()));

    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let dst = 0x4000u64;

    controller.mmio_write_u32(
        &mut mem,
        registers::HBA_GHC,
        registers::GHC_AE | registers::GHC_IE,
    );
    controller.mmio_write_u32(
        &mut mem,
        registers::HBA_PORTS_BASE + registers::PX_CLB,
        clb as u32,
    );
    controller.mmio_write_u32(
        &mut mem,
        registers::HBA_PORTS_BASE + registers::PX_FB,
        fb as u32,
    );
    controller.mmio_write_u32(
        &mut mem,
        registers::HBA_PORTS_BASE + registers::PX_IE,
        registers::PXIE_DHRE,
    );
    controller.mmio_write_u32(
        &mut mem,
        registers::HBA_PORTS_BASE + registers::PX_CMD,
        registers::PXCMD_FRE | registers::PXCMD_ST | registers::PXCMD_SUD,
    );

    let snap = controller.save_state();

    let mut restored = AhciController::new(Box::new(shared_disk.clone()));
    restored.load_state(&snap).unwrap();

    assert_eq!(
        restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CLB) as u64
            | ((restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CLBU)
                as u64)
                << 32),
        clb
    );
    assert_eq!(
        restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_FB) as u64
            | ((restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_FBU)
                as u64)
                << 32),
        fb
    );

    // Prepare a READ DMA EXT command.
    let header = build_cmd_header(5, false, 1, ctba);
    mem.write_physical(clb, &header);
    write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 2, 1);
    write_prd(&mut mem, ctba + 0x80, dst, 512);
    mem.write_physical(dst, &[0xaa; 512]);

    // Without re-attaching a disk, the canonical controller leaves the command pending and does
    // not DMA.
    restored.mmio_write_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CI, 1);
    assert_eq!(
        restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CI) & 1,
        1
    );

    let mut got = [0u8; 512];
    mem.read_physical(dst, &mut got);
    assert_eq!(got, [0xaa; 512]);

    // Re-attach the disk and trigger processing on the next MMIO access.
    restored.attach_disk(Box::new(shared_disk));
    let _ = restored.mmio_read_u32(&mut mem, registers::HBA_IS);

    assert_eq!(
        restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CI) & 1,
        0
    );

    mem.read_physical(dst, &mut got);
    let disk_guard = disk.lock().unwrap();
    let expected = &disk_guard.data()[2 * 512..3 * 512];
    assert_eq!(&got[..], expected);
}
