use std::io;

use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3, SATA_AHCI_ICH9};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_pc_platform::{PcPlatform, Windows7StorageTopologyConfig};
use aero_storage::{MemBackend, RawDisk, VirtualDisk as _, SECTOR_SIZE};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io.write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io.write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

struct MemIso {
    bytes: Vec<u8>,
}

impl IsoBackend for MemIso {
    fn sector_count(&self) -> u32 {
        (self.bytes.len() / 2048) as u32
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        if !buf.len().is_multiple_of(2048) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "buf length not multiple of 2048",
            ));
        }
        let byte_off = (lba as usize)
            .checked_mul(2048)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        let byte_end = byte_off
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        if byte_end > self.bytes.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past EOF"));
        }
        buf.copy_from_slice(&self.bytes[byte_off..byte_end]);
        Ok(())
    }
}

fn ahci_bar5_base(pc: &mut PcPlatform) -> u64 {
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x24);
    u64::from(bar5 & 0xffff_fff0)
}

#[test]
fn win7_storage_topology_is_canonical_and_reads_hdd_and_cdrom() {
    // HDD on AHCI port 0: LBA 4 contains marker bytes [9,8,7,6].
    let capacity = 64 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector = vec![0u8; SECTOR_SIZE];
    sector[0..4].copy_from_slice(&[9, 8, 7, 6]);
    disk.write_sectors(4, &sector).unwrap();
    let hdd = AtaDrive::new(Box::new(disk)).unwrap();

    // CD-ROM: sector 0 (2048-byte) starts with [1,2,3,4].
    let mut iso_bytes = vec![0u8; 2048 * 4];
    iso_bytes[0..4].copy_from_slice(&[1, 2, 3, 4]);
    let cdrom = AtapiCdrom::new(Some(Box::new(MemIso { bytes: iso_bytes })));

    let mut pc = PcPlatform::new_with_windows7_storage_topology(
        2 * 1024 * 1024,
        Windows7StorageTopologyConfig { hdd, cdrom },
    );

    // --- PCI enumeration: BDFs + class codes ---
    {
        // 00:01.0 ISA bridge (multifunction parent for the IDE function)
        let bdf = ISA_PIIX3.bdf;
        let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(ISA_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(ISA_PIIX3.device_id));

        let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
        assert_eq!((class >> 24) as u8, 0x06);
        assert_eq!((class >> 16) as u8, 0x01);
    }

    {
        // 00:01.1 PIIX3 IDE
        let bdf = IDE_PIIX3.bdf;
        let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(IDE_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(IDE_PIIX3.device_id));

        let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
        assert_eq!((class >> 24) as u8, 0x01);
        assert_eq!((class >> 16) as u8, 0x01);
        assert_eq!((class >> 8) as u8, 0x8a);

        let intx = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);
        let int_line = (intx & 0xff) as u8;
        let int_pin = ((intx >> 8) & 0xff) as u8;
        assert_eq!(int_pin, 1, "IDE should expose INTA# in PCI config space");
        assert_eq!(int_line, 11, "default PIRQ swizzle routes 00:01.1 INTA# to GSI11");

        // Legacy-compatible BARs.
        let bar0 = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10);
        let bar1 = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x14);
        let bar2 = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x18);
        let bar3 = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x1c);
        assert_eq!(bar0 & 0xffff_fffc, 0x0000_01f0);
        assert_eq!(bar1 & 0xffff_fffc, 0x0000_03f4);
        assert_eq!(bar2 & 0xffff_fffc, 0x0000_0170);
        assert_eq!(bar3 & 0xffff_fffc, 0x0000_0374);
    }

    {
        // 00:02.0 ICH9 AHCI
        let bdf = SATA_AHCI_ICH9.bdf;
        let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(SATA_AHCI_ICH9.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(SATA_AHCI_ICH9.device_id));

        let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
        assert_eq!((class >> 24) as u8, 0x01);
        assert_eq!((class >> 16) as u8, 0x06);
        assert_eq!((class >> 8) as u8, 0x01);

        let intx = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);
        let int_line = (intx & 0xff) as u8;
        let int_pin = ((intx >> 8) & 0xff) as u8;
        assert_eq!(int_pin, 1, "AHCI should expose INTA# in PCI config space");
        assert_eq!(int_line, 12, "default PIRQ swizzle routes 00:02.0 INTA# to GSI12");

        let bar5 = ahci_bar5_base(&mut pc);
        assert_ne!(bar5, 0);
        assert_eq!(bar5 % 0x2000, 0);
    }

    // --- AHCI: READ DMA EXT of one 512-byte sector ---
    {
        let bdf = SATA_AHCI_ICH9.bdf;
        // Enable bus mastering so the controller can DMA the command list and data.
        write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

        let bar5 = ahci_bar5_base(&mut pc);

        // Program HBA + port registers.
        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let data_buf = 0x4000u64;

        // PxCLB / PxFB (lower 32 bits; high is zero).
        pc.memory.write_u32(bar5 + 0x100, clb as u32);
        pc.memory.write_u32(bar5 + 0x100 + 0x08, fb as u32);

        // GHC: AE + IE.
        pc.memory.write_u32(bar5 + 0x04, 0x8000_0002);
        // PxIE: DHRS.
        pc.memory.write_u32(bar5 + 0x100 + 0x14, 0x0000_0001);
        // PxCMD: ST + FRE.
        pc.memory.write_u32(bar5 + 0x100 + 0x18, 0x0000_0011);

        // Command header (slot 0).
        // flags: CFL=5, PRDTL=1.
        pc.memory.write_u32(clb, 0x0001_0005);
        pc.memory.write_u32(clb + 0x04, 0); // PRDBC
        pc.memory.write_u32(clb + 0x08, ctba as u32);
        pc.memory.write_u32(clb + 0x0c, 0);

        // CFIS at CTBA (64 bytes); use READ DMA EXT (0x25).
        let mut cfis = [0u8; 64];
        cfis[0] = 0x27; // Register H2D
        cfis[1] = 0x80; // C bit
        cfis[2] = 0x25; // READ DMA EXT
        cfis[7] = 0x40; // LBA mode

        let lba = 4u64;
        cfis[4] = (lba & 0xff) as u8;
        cfis[5] = ((lba >> 8) & 0xff) as u8;
        cfis[6] = ((lba >> 16) & 0xff) as u8;
        cfis[8] = ((lba >> 24) & 0xff) as u8;
        cfis[9] = ((lba >> 32) & 0xff) as u8;
        cfis[10] = ((lba >> 40) & 0xff) as u8;

        cfis[12] = 1; // sector count (low)
        cfis[13] = 0; // sector count (high)
        pc.memory.write_physical(ctba, &cfis);

        // PRDT entry 0 at CTBA+0x80.
        pc.memory.write_u32(ctba + 0x80, data_buf as u32);
        pc.memory.write_u32(ctba + 0x84, 0);
        pc.memory.write_u32(ctba + 0x88, 0);
        // DBC is byte_count-1 in bits 0..21.
        pc.memory.write_u32(ctba + 0x8c, (SECTOR_SIZE as u32 - 1) & 0x003f_ffff);

        // Issue slot 0.
        pc.memory.write_u32(bar5 + 0x100 + 0x38, 1);
        pc.process_ahci();

        let mut got = [0u8; 4];
        pc.memory.read_physical(data_buf, &mut got);
        assert_eq!(got, [9, 8, 7, 6]);
    }

    // --- IDE/ATAPI (secondary master): PACKET READ(10) of one 2048-byte sector ---
    {
        // The ATAPI model surfaces "unit attention / medium changed" once after media insertion.
        // Clear it via TEST UNIT READY + REQUEST SENSE, then issue the READ(10).

        const IDE_BASE: u16 = 0x170;
        const IDE_DEVICE: u16 = IDE_BASE + 6;
        const IDE_COMMAND: u16 = IDE_BASE + 7;
        const IDE_LBA1: u16 = IDE_BASE + 4;
        const IDE_LBA2: u16 = IDE_BASE + 5;
        const IDE_DATA: u16 = IDE_BASE;

        // Select secondary master.
        pc.io.write_u8(IDE_DEVICE, 0xA0);

        // --- TEST UNIT READY (expect error the first time due to "media changed") ---
        pc.io.write_u8(IDE_LBA1, 0x00);
        pc.io.write_u8(IDE_LBA2, 0x00);
        pc.io.write_u8(IDE_COMMAND, 0xA0);
        // Packet is all zeros (opcode 0x00).
        for _ in 0..6 {
            pc.io.write(IDE_DATA, 2, 0);
        }

        // --- REQUEST SENSE (18 bytes) ---
        pc.io.write_u8(IDE_LBA1, 0x12);
        pc.io.write_u8(IDE_LBA2, 0x00);
        pc.io.write_u8(IDE_COMMAND, 0xA0);
        let mut sense_pkt = [0u8; 12];
        sense_pkt[0] = 0x03;
        sense_pkt[4] = 18;
        for i in 0..6 {
            let w = u16::from_le_bytes([sense_pkt[i * 2], sense_pkt[i * 2 + 1]]);
            pc.io.write(IDE_DATA, 2, u32::from(w));
        }
        // Drain returned sense data (PIO IN).
        for _ in 0..(18 / 2) {
            let _ = pc.io.read(IDE_DATA, 2);
        }

        // --- READ(10): one 2048-byte sector ---
        pc.io.write_u8(IDE_LBA1, 0x00);
        pc.io.write_u8(IDE_LBA2, 0x08);
        pc.io.write_u8(IDE_COMMAND, 0xA0);

        // SCSI READ(10) packet: LBA=0, blocks=1.
        let mut read_pkt = [0u8; 12];
        read_pkt[0] = 0x28;
        read_pkt[7..9].copy_from_slice(&1u16.to_be_bytes());

        for i in 0..6 {
            let w = u16::from_le_bytes([read_pkt[i * 2], read_pkt[i * 2 + 1]]);
            pc.io.write(IDE_DATA, 2, u32::from(w));
        }

        let mut buf = vec![0u8; 2048];
        for i in 0..(buf.len() / 2) {
            let w = pc.io.read(IDE_DATA, 2) as u16;
            let b = w.to_le_bytes();
            buf[i * 2] = b[0];
            buf[i * 2 + 1] = b[1];
        }

        assert_eq!(&buf[0..4], &[1, 2, 3, 4]);
    }
}
