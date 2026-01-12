#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use aero_devices_nvme::{DiskBackend, DiskError, DiskResult, NvmeController};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
#[cfg(not(target_arch = "wasm32"))]
use emulator::io::storage::ahci::{registers as ahci_regs, AhciController};
#[cfg(not(target_arch = "wasm32"))]
use emulator::io::storage::disk as emu_disk;
#[cfg(not(target_arch = "wasm32"))]
use memory::MemoryBus;

#[cfg(not(target_arch = "wasm32"))]
const PAGE_SIZE: usize = 4096;

#[cfg(not(target_arch = "wasm32"))]
struct BenchMem {
    buf: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl BenchMem {
    fn new(size: usize) -> Self {
        Self {
            buf: vec![0u8; size],
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl MemoryBus for BenchMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        let end = start + buf.len();
        buf.copy_from_slice(&self.buf[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        let end = start + buf.len();
        self.buf[start..end].copy_from_slice(buf);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_scatter_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("scatter_copy");
    for size_kib in [4usize, 64, 256, 1024] {
        let len = size_kib * 1024;
        let src = vec![0x55u8; len];

        // NVMe-ish: 4KiB pages via PRPs.
        group.bench_with_input(BenchmarkId::new("nvme_prp", size_kib), &len, |b, _| {
            let mut mem = BenchMem::new(8 * 1024 * 1024);
            let base = 0x10000u64;
            b.iter(|| {
                let mut offset = 0usize;
                let mut paddr = base;
                while offset < src.len() {
                    let chunk = (src.len() - offset).min(PAGE_SIZE);
                    mem.write_physical(paddr, &src[offset..offset + chunk]);
                    offset += chunk;
                    paddr += PAGE_SIZE as u64;
                }
            })
        });

        // AHCI-ish: pretend we have 1KiB PRD segments.
        group.bench_with_input(BenchmarkId::new("ahci_prdt", size_kib), &len, |b, _| {
            let mut mem = BenchMem::new(8 * 1024 * 1024);
            let base = 0x10000u64;
            b.iter(|| {
                let mut offset = 0usize;
                let mut paddr = base;
                while offset < src.len() {
                    let chunk = (src.len() - offset).min(1024);
                    mem.write_physical(paddr, &src[offset..offset + chunk]);
                    offset += chunk;
                    paddr += 1024;
                }
            })
        });
    }
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
struct VecDisk {
    sector_size: u32,
    data: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl VecDisk {
    fn new(total_sectors: u64) -> Self {
        let sector_size = 512u32;
        let len = usize::try_from(total_sectors * u64::from(sector_size)).expect("disk too large");
        Self {
            sector_size,
            data: vec![0u8; len],
        }
    }

    fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn check(&self, lba: u64, bytes: usize) -> DiskResult<()> {
        if !bytes.is_multiple_of(self.sector_size as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: bytes,
                sector_size: self.sector_size,
            });
        }
        let sectors = (bytes / self.sector_size as usize) as u64;
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors(),
        })?;
        let capacity = self.total_sectors();
        if end > capacity {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: capacity,
            });
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl DiskBackend for VecDisk {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        (self.data.len() as u64) / u64::from(self.sector_size)
    }

    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> DiskResult<()> {
        self.check(lba, buffer.len())?;
        let offset = usize::try_from(lba * u64::from(self.sector_size)).expect("offset too large");
        buffer.copy_from_slice(&self.data[offset..offset + buffer.len()]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> DiskResult<()> {
        self.check(lba, buffer.len())?;
        let offset = usize::try_from(lba * u64::from(self.sector_size)).expect("offset too large");
        self.data[offset..offset + buffer.len()].copy_from_slice(buffer);
        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn write_nvme_cmd(mem: &mut dyn MemoryBus, addr: u64, cmd: &[u8; 64]) {
    mem.write_physical(addr, cmd);
}

#[cfg(not(target_arch = "wasm32"))]
fn set_cmd_u16(cmd: &mut [u8; 64], offset: usize, val: u16) {
    cmd[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
}

#[cfg(not(target_arch = "wasm32"))]
fn set_cmd_u32(cmd: &mut [u8; 64], offset: usize, val: u32) {
    cmd[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[cfg(not(target_arch = "wasm32"))]
fn set_cmd_u64(cmd: &mut [u8; 64], offset: usize, val: u64) {
    cmd[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_device_read_4k(c: &mut Criterion) {
    const ASQ: u64 = 0x10_000;
    const ACQ: u64 = 0x20_000;
    const IO_CQ: u64 = 0x40_000;
    const IO_SQ: u64 = 0x50_000;
    const DATA: u64 = 0x60_000;

    let mut group = c.benchmark_group("read_4k_device_path");

    group.bench_function("nvme_read_4k", |b| {
        let mut mem = BenchMem::new(2 * 1024 * 1024);
        let mut disk = VecDisk::new(16 * 1024);
        for (i, byte) in disk.data_mut().iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(31);
        }

        let mut ctrl = NvmeController::new(Box::new(disk));

        // Admin SQ/CQ setup and enable.
        ctrl.mmio_write(0x0024, 4, 0x000f_000f);
        ctrl.mmio_write(0x0028, 8, ASQ);
        ctrl.mmio_write(0x0030, 8, ACQ);
        ctrl.mmio_write(0x0014, 4, 1);

        // Create IO CQ (qid=1).
        let mut cmd = [0u8; 64];
        cmd[0] = 0x05;
        set_cmd_u16(&mut cmd, 2, 1); // cid
        set_cmd_u64(&mut cmd, 24, IO_CQ);
        set_cmd_u32(&mut cmd, 40, (63u32 << 16) | 1); // qsize=64, qid=1
        set_cmd_u32(&mut cmd, 44, 0x3); // PC + IEN
        write_nvme_cmd(&mut mem, ASQ, &cmd);
        ctrl.mmio_write(0x1000, 4, 1);
        ctrl.process(&mut mem);

        // Create IO SQ (qid=1, cqid=1).
        let mut cmd = [0u8; 64];
        cmd[0] = 0x01;
        set_cmd_u16(&mut cmd, 2, 2); // cid
        set_cmd_u64(&mut cmd, 24, IO_SQ);
        set_cmd_u32(&mut cmd, 40, (63u32 << 16) | 1); // qsize=64, qid=1
        set_cmd_u32(&mut cmd, 44, 1); // cqid=1
        write_nvme_cmd(&mut mem, ASQ + 64, &cmd);
        ctrl.mmio_write(0x1000, 4, 2);
        ctrl.process(&mut mem);

        // Consume the two admin completions so INTx doesn't stay asserted.
        ctrl.mmio_write(0x1004, 4, 2);

        let mut sq_tail: u16 = 0;
        let mut cq_head: u16 = 0;
        let mut cid: u16 = 0x1000;

        b.iter(|| {
            let slot = sq_tail;
            let mut cmd = [0u8; 64];
            cmd[0] = 0x02; // READ
            set_cmd_u16(&mut cmd, 2, cid);
            set_cmd_u32(&mut cmd, 4, 1); // nsid
            set_cmd_u64(&mut cmd, 24, DATA);
            set_cmd_u32(&mut cmd, 48, 7); // NLB = 8 sectors - 1
            write_nvme_cmd(&mut mem, IO_SQ + u64::from(slot) * 64, &cmd);

            sq_tail = (sq_tail + 1) % 64;
            ctrl.mmio_write(0x1008, 4, u64::from(sq_tail));
            ctrl.process(&mut mem);

            cq_head = (cq_head + 1) % 64;
            ctrl.mmio_write(0x100c, 4, u64::from(cq_head));

            cid = cid.wrapping_add(1);
        });
    });

    group.bench_function("ahci_read_4k", |b| {
        let mut mem = BenchMem::new(2 * 1024 * 1024);
        let mut disk = emu_disk::MemDisk::new(16 * 1024);
        for (i, byte) in disk.data_mut().iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(31);
        }

        let mut ahci = AhciController::new(Box::new(disk));

        let clb = 0x10_000u64;
        let fb = 0x11_000u64;
        let ctba = 0x12_000u64;
        let dst = 0x13_000u64;

        ahci.mmio_write_u32(
            &mut mem,
            ahci_regs::HBA_GHC,
            ahci_regs::GHC_AE | ahci_regs::GHC_IE,
        );
        ahci.mmio_write_u32(
            &mut mem,
            ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_CLB,
            clb as u32,
        );
        ahci.mmio_write_u32(
            &mut mem,
            ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_CLBU,
            (clb >> 32) as u32,
        );
        ahci.mmio_write_u32(
            &mut mem,
            ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_FB,
            fb as u32,
        );
        ahci.mmio_write_u32(
            &mut mem,
            ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_FBU,
            (fb >> 32) as u32,
        );
        ahci.mmio_write_u32(
            &mut mem,
            ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_IE,
            ahci_regs::PXIE_DHRE,
        );
        ahci.mmio_write_u32(
            &mut mem,
            ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_CMD,
            ahci_regs::PXCMD_FRE | ahci_regs::PXCMD_ST | ahci_regs::PXCMD_SUD,
        );

        // Command header for slot 0 (CFIS len = 20 bytes = 5 dwords, PRDT len = 1).
        let dw0 = 5u32 | (1u32 << 16);
        mem.write_u32(clb, dw0);
        mem.write_u32(clb + 4, 0);
        mem.write_u32(clb + 8, ctba as u32);
        mem.write_u32(clb + 12, (ctba >> 32) as u32);

        // CFIS: READ_DMA_EXT, LBA=0, count=8 sectors.
        let mut fis = [0u8; 64];
        fis[0] = 0x27;
        fis[1] = 0x80;
        fis[2] = 0x25;
        fis[7] = 0x40;
        fis[12] = 8;
        mem.write_physical(ctba, &fis);

        // PRD: one 4KiB buffer at dst.
        let dbc = (4096u32 - 1) | (1u32 << 31);
        mem.write_u32(ctba + 0x80, dst as u32);
        mem.write_u32(ctba + 0x84, (dst >> 32) as u32);
        mem.write_u32(ctba + 0x8c, dbc);

        b.iter(|| {
            ahci.mmio_write_u32(&mut mem, ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_CI, 1);
            ahci.mmio_write_u32(
                &mut mem,
                ahci_regs::HBA_PORTS_BASE + ahci_regs::PX_IS,
                ahci_regs::PXIS_DHRS,
            );
            ahci.mmio_write_u32(&mut mem, ahci_regs::HBA_IS, 1);
        });
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(benches, bench_scatter_copy, bench_device_read_4k);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
