use aero_bios::firmware::{BlockDevice, DiskError, Keyboard, Memory};
use aero_bios::types::{E820_TYPE_ACPI, E820_TYPE_NVS, E820_TYPE_RAM, E820_TYPE_RESERVED};
use aero_bios::{Bios, BiosConfig, RealModeCpu};
use aero_acpi::AcpiTables;
use std::collections::VecDeque;

struct TestMemory {
    bytes: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }

    fn read_u64(&self, paddr: u32) -> u64 {
        let lo = self.read_u32(paddr) as u64;
        let hi = self.read_u32(paddr + 4) as u64;
        lo | (hi << 32)
    }
}

impl Memory for TestMemory {
    fn read_u8(&self, paddr: u32) -> u8 {
        self.bytes[paddr as usize]
    }

    fn read_u16(&self, paddr: u32) -> u16 {
        let lo = self.read_u8(paddr) as u16;
        let hi = self.read_u8(paddr + 1) as u16;
        lo | (hi << 8)
    }

    fn read_u32(&self, paddr: u32) -> u32 {
        let b0 = self.read_u8(paddr) as u32;
        let b1 = self.read_u8(paddr + 1) as u32;
        let b2 = self.read_u8(paddr + 2) as u32;
        let b3 = self.read_u8(paddr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn write_u8(&mut self, paddr: u32, v: u8) {
        self.bytes[paddr as usize] = v;
    }

    fn write_u16(&mut self, paddr: u32, v: u16) {
        self.write_u8(paddr, v as u8);
        self.write_u8(paddr + 1, (v >> 8) as u8);
    }

    fn write_u32(&mut self, paddr: u32, v: u32) {
        self.write_u8(paddr, v as u8);
        self.write_u8(paddr + 1, (v >> 8) as u8);
        self.write_u8(paddr + 2, (v >> 16) as u8);
        self.write_u8(paddr + 3, (v >> 24) as u8);
    }
}

struct VecDisk {
    bytes: Vec<u8>,
    read_only: bool,
}

impl VecDisk {
    fn new(mut bytes: Vec<u8>) -> Self {
        assert_eq!(bytes.len() % 512, 0);
        if bytes.is_empty() {
            bytes.resize(512, 0);
        }
        Self {
            bytes,
            read_only: false,
        }
    }

    fn set_read_only(mut self, ro: bool) -> Self {
        self.read_only = ro;
        self
    }
}

impl BlockDevice for VecDisk {
    fn read_sector(&mut self, lba: u64, buf512: &mut [u8; 512]) -> Result<(), DiskError> {
        let start = usize::try_from(lba * 512).map_err(|_| DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let slice = self.bytes.get(start..end).ok_or(DiskError::OutOfRange)?;
        buf512.copy_from_slice(slice);
        Ok(())
    }

    fn write_sector(&mut self, lba: u64, buf512: &[u8; 512]) -> Result<(), DiskError> {
        if self.read_only {
            return Err(DiskError::ReadOnly);
        }
        let start = usize::try_from(lba * 512).map_err(|_| DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let slice = self.bytes.get_mut(start..end).ok_or(DiskError::OutOfRange)?;
        slice.copy_from_slice(buf512);
        Ok(())
    }

    fn sector_count(&self) -> u64 {
        (self.bytes.len() / 512) as u64
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }
}

#[derive(Default)]
struct TestKeyboard {
    queue: VecDeque<u16>,
}

impl TestKeyboard {
    fn push(&mut self, ascii: u8, scan: u8) {
        self.queue.push_back(((scan as u16) << 8) | ascii as u16);
    }
}

impl Keyboard for TestKeyboard {
    fn pop_key(&mut self) -> Option<u16> {
        self.queue.pop_front()
    }

    fn peek_key(&mut self) -> Option<u16> {
        self.queue.front().copied()
    }
}

fn acpi_reclaimable_region_from_tables(tables: &AcpiTables) -> (u64, u64) {
    let addrs = &tables.addresses;
    let mut start = addrs.dsdt;
    start = start.min(addrs.fadt);
    start = start.min(addrs.madt);
    start = start.min(addrs.hpet);
    start = start.min(addrs.rsdt);
    start = start.min(addrs.xsdt);

    let mut end = start;
    end = end.max(addrs.dsdt + tables.dsdt.len() as u64);
    end = end.max(addrs.fadt + tables.fadt.len() as u64);
    end = end.max(addrs.madt + tables.madt.len() as u64);
    end = end.max(addrs.hpet + tables.hpet.len() as u64);
    end = end.max(addrs.rsdt + tables.rsdt.len() as u64);
    end = end.max(addrs.xsdt + tables.xsdt.len() as u64);

    (start, end.saturating_sub(start))
}

fn read_e820_entry(mem: &TestMemory, paddr: u32) -> (u64, u64, u32, u32) {
    let base = mem.read_u64(paddr);
    let len = mem.read_u64(paddr + 8);
    let region_type = mem.read_u32(paddr + 16);
    let ext = mem.read_u32(paddr + 20);
    (base, len, region_type, ext)
}

#[test]
fn int16_returns_key_and_sets_zf_when_empty() {
    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);
    let mut disk = VecDisk::new(vec![0; 512]);
    let mut kbd = TestKeyboard::default();

    kbd.push(b'A', 0x1E);

    cpu.set_ah(0x00);
    bios.handle_interrupt(0x16, &mut cpu, &mut mem, &mut disk, &mut kbd);

    assert_eq!(cpu.ax(), 0x1E41);
    assert!(!cpu.cf());
    assert!(!cpu.zf());

    // Now empty: BIOS returns ZF=1 and AX=0 (non-blocking semantics for tests).
    cpu.set_ah(0x00);
    bios.handle_interrupt(0x16, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert_eq!(cpu.ax(), 0);
    assert!(!cpu.cf());
    assert!(cpu.zf());
}

#[test]
fn int13_chs_read_copies_sector_to_memory() {
    let mut disk_bytes = vec![0u8; 2 * 512];
    disk_bytes[512] = 0x42;
    let mut disk = VecDisk::new(disk_bytes);

    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);
    let mut kbd = TestKeyboard::default();

    cpu.es = 0;
    cpu.set_bx(0x0500);

    cpu.set_ax(0x0201); // AH=2 read, AL=1 sector
    cpu.set_cx(0x0002); // CHS: cyl=0, sector=2
    cpu.set_dh(0); // head 0

    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.ah(), 0);
    assert_eq!(mem.read_u8(0x0500), 0x42);

    // Invalid sector (sector number 0) should fail.
    cpu.set_ax(0x0201);
    cpu.set_cx(0x0000);
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(cpu.cf());
    assert_eq!(cpu.ah(), 0x01);
}

#[test]
fn int13_extended_read_uses_dap_structure() {
    let mut disk_bytes = vec![0u8; 3 * 512];
    disk_bytes[2 * 512] = 0x99;
    let mut disk = VecDisk::new(disk_bytes);

    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);
    let mut kbd = TestKeyboard::default();

    // DAP at 0000:0600. Buffer at 0000:0700. LBA=2, sectors=1.
    let dap = 0x0600u32;
    mem.write_u8(dap, 0x10); // size
    mem.write_u8(dap + 1, 0); // reserved
    mem.write_u16(dap + 2, 1); // sectors
    mem.write_u16(dap + 4, 0x0700); // buffer offset
    mem.write_u16(dap + 6, 0x0000); // buffer segment
    mem.write_u32(dap + 8, 2); // lba low
    mem.write_u32(dap + 12, 0); // lba high

    cpu.ds = 0;
    cpu.esi = dap;
    cpu.set_ah(0x42);

    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut disk, &mut kbd);

    assert!(!cpu.cf());
    assert_eq!(cpu.ah(), 0);
    assert_eq!(mem.read_u8(0x0700), 0x99);

    // Extended write should fail if disk is read-only.
    let mut ro_disk = VecDisk::new(vec![0u8; 512]).set_read_only(true);
    cpu.set_ah(0x43);
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut ro_disk, &mut kbd);
    assert!(cpu.cf());
    assert_eq!(cpu.ah(), 0x03);
}

#[test]
fn int15_e820_enumeration_is_non_overlapping_and_includes_acpi_region() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(8 * 1024 * 1024);
    let mut disk = VecDisk::new(vec![0; 512]);
    let mut kbd = TestKeyboard::default();

    let mut entries = Vec::new();
    cpu.edx = 0x534D_4150; // 'SMAP'
    cpu.ecx = 24;
    cpu.ebx = 0;
    cpu.es = 0;
    cpu.edi = 0x0600;

    loop {
        // Caller must set EAX=0xE820 on each invocation (BIOS returns 'SMAP').
        cpu.eax = 0xE820;
        bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
        assert!(
            !cpu.cf(),
            "E820 call failed at index {}, cpu={:?}",
            cpu.ebx,
            cpu
        );
        let entry = read_e820_entry(&mem, 0x0600);
        entries.push(entry);
        if cpu.ebx == 0 {
            break;
        }
    }

    assert!(!entries.is_empty());

    // Sort by base and check for overlaps.
    entries.sort_by_key(|e| e.0);
    for win in entries.windows(2) {
        let (base, len, _, _) = win[0];
        let end = base + len;
        let (next_base, _, _, _) = win[1];
        assert!(
            next_base >= end,
            "E820 overlap: [{base:#x}-{end:#x}) overlaps next base {next_base:#x}"
        );
    }

    assert!(
        entries.iter().any(|e| e.2 == E820_TYPE_RAM),
        "expected at least one RAM entry"
    );
    assert!(
        entries.iter().any(|e| e.2 == E820_TYPE_RESERVED),
        "expected at least one RESERVED entry"
    );

    // Low-memory reserved region should be present.
    assert!(
        entries
            .iter()
            .any(|e| e.0 == 0x0009_F000 && e.2 == E820_TYPE_RESERVED),
        "expected 0x9F000..1MiB to be reserved"
    );

    // ACPI tables (if enabled) should be marked as ACPI in E820.
    let Some(acpi) = bios.acpi_tables() else {
        panic!("default BIOS config should enable ACPI tables");
    };
    let (acpi_base, acpi_len) = acpi_reclaimable_region_from_tables(acpi);
    assert!(
        entries
            .iter()
            .any(|e| e.0 == acpi_base && e.1 == acpi_len && e.2 == E820_TYPE_ACPI),
        "expected ACPI E820 region [{acpi_base:#x}-{:#x})",
        acpi_base + acpi_len
    );

    // The FACS must live in ACPI NVS memory, not in the reclaimable SDT blob.
    let facs_addr = acpi.addresses.facs;
    assert!(
        entries.iter().any(|e| {
            e.2 == E820_TYPE_NVS
                && facs_addr >= e.0
                && facs_addr + (acpi.facs.len() as u64) <= e.0 + e.1
        }),
        "expected ACPI NVS E820 region covering FACS at {facs_addr:#x}"
    );
}
