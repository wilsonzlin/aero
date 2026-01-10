use aero_bios::firmware::{BlockDevice, DiskError, Keyboard, Memory, NullKeyboard};
use aero_bios::{build_bios_rom, Bios, BiosConfig, RealModeCpu, FLAG_IF};

struct SimpleMemory {
    bytes: Vec<u8>,
}

impl SimpleMemory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }
}

impl Memory for SimpleMemory {
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
}

impl VecDisk {
    fn new(bytes: Vec<u8>) -> Self {
        assert_eq!(bytes.len() % 512, 0);
        Self { bytes }
    }
}

impl BlockDevice for VecDisk {
    fn read_sector(&mut self, lba: u64, buf512: &mut [u8; 512]) -> Result<(), DiskError> {
        let start = lba
            .checked_mul(512)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let slice = self.bytes.get(start..end).ok_or(DiskError::OutOfRange)?;
        buf512.copy_from_slice(slice);
        Ok(())
    }

    fn write_sector(&mut self, lba: u64, buf512: &[u8; 512]) -> Result<(), DiskError> {
        let start = lba
            .checked_mul(512)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let slice = self
            .bytes
            .get_mut(start..end)
            .ok_or(DiskError::OutOfRange)?;
        slice.copy_from_slice(buf512);
        Ok(())
    }

    fn sector_count(&self) -> u64 {
        (self.bytes.len() / 512) as u64
    }
}

fn make_test_boot_sector() -> [u8; 512] {
    // A tiny boot sector that prints "BOOTED" using INT 10h teletype and then
    // writes 0x42 to 0x0000:0x0500 as an execution marker before halting.
    //
    // The code assumes BIOS jumps to CS:IP = 0000:7C00 and sets DS=0.
    let mut s = [0u8; 512];

    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[
        0xFA, // cli
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0x8E, 0xC0, // mov es, ax
    ]);

    // mov si, imm16 (patched below)
    code.extend_from_slice(&[0xBE, 0x00, 0x00]);
    let si_imm_index = code.len() - 2;

    // .loop:
    let loop_off = code.len();
    code.extend_from_slice(&[
        0xAC, // lodsb
        0x08, 0xC0, // or al, al
        0x74, 0x00, // jz .done (patched below)
    ]);
    let jz_rel_index = code.len() - 1;

    code.extend_from_slice(&[
        0xB4, 0x0E, // mov ah, 0x0E
        0xBB, 0x07, 0x00, // mov bx, 0x0007
        0xCD, 0x10, // int 10h
        0xEB, 0x00, // jmp .loop (patched below)
    ]);
    let jmp_rel_index = code.len() - 1;

    // .done:
    let done_off = code.len();
    code.extend_from_slice(&[
        0xC6, 0x06, 0x00, 0x05, 0x42, // mov byte [0x0500], 0x42
        0xF4, // hlt
    ]);

    // msg:
    let msg_offset = code.len() as u16;
    let msg_phys = 0x7C00u16.wrapping_add(msg_offset);

    // Patch mov si, imm16.
    code[si_imm_index] = (msg_phys & 0xFF) as u8;
    code[si_imm_index + 1] = (msg_phys >> 8) as u8;

    // Append message.
    code.extend_from_slice(b"BOOTED\0");

    // Patch JZ rel8: from next IP after JZ (offset of instruction end) to done label.
    let jz_opcode_off = jz_rel_index - 1;
    let jz_next = jz_opcode_off as i32 + 2;
    let rel = done_off as i32 - jz_next;
    code[jz_rel_index] = rel as i8 as u8;

    // Patch JMP rel8 back to loop start (offset of lodsb).
    let jmp_opcode_off = jmp_rel_index - 1;
    let jmp_next = jmp_opcode_off as i32 + 2;
    let rel2 = loop_off as i32 - jmp_next;
    code[jmp_rel_index] = rel2 as i8 as u8;

    assert!(code.len() <= 510);
    s[..code.len()].copy_from_slice(&code);
    s[510] = 0x55;
    s[511] = 0xAA;
    s
}

fn run_real_mode_program(
    bios: &mut Bios,
    cpu: &mut RealModeCpu,
    mem: &mut SimpleMemory,
    disk: &mut VecDisk,
    kbd: &mut impl Keyboard,
) {
    // Extremely small instruction subset interpreter sufficient for the test
    // boot sector. This is intentionally *not* a general x86 emulator.
    for _step in 0..100_000u32 {
        let ip = cpu.ip();
        let paddr = cpu.cs_base().wrapping_add(ip as u32);
        let op = mem.read_u8(paddr);
        match op {
            0xFA => {
                // CLI
                cpu.eflags &= !FLAG_IF;
                cpu.set_ip(ip.wrapping_add(1));
            }
            0x31 => {
                // XOR r/m16, r16 (only 31 C0: xor ax, ax)
                let modrm = mem.read_u8(paddr + 1);
                assert_eq!(modrm, 0xC0);
                cpu.set_ax(0);
                cpu.set_zf(true);
                cpu.set_cf(false);
                cpu.set_ip(ip.wrapping_add(2));
            }
            0x8E => {
                // MOV Sreg, r/m16 (only ds<-ax and es<-ax)
                let modrm = mem.read_u8(paddr + 1);
                match modrm {
                    0xD8 => cpu.ds = cpu.ax(), // mov ds, ax
                    0xC0 => cpu.es = cpu.ax(), // mov es, ax
                    _ => panic!("unsupported modrm for 8E: {modrm:02x}"),
                }
                cpu.set_ip(ip.wrapping_add(2));
            }
            0xBE => {
                // MOV SI, imm16
                let imm = mem.read_u16(paddr + 1);
                cpu.esi = imm as u32;
                cpu.set_ip(ip.wrapping_add(3));
            }
            0xAC => {
                // LODSB
                let addr = cpu.ds_base().wrapping_add(cpu.esi as u32);
                let b = mem.read_u8(addr);
                cpu.set_al(b);
                cpu.esi = cpu.esi.wrapping_add(1);
                cpu.set_ip(ip.wrapping_add(1));
            }
            0x08 => {
                // OR r/m8, r8 (only 08 C0: or al, al)
                let modrm = mem.read_u8(paddr + 1);
                assert_eq!(modrm, 0xC0);
                let al = cpu.al();
                let res = al | al;
                cpu.set_al(res);
                cpu.set_zf(res == 0);
                cpu.set_cf(false);
                cpu.set_ip(ip.wrapping_add(2));
            }
            0x74 => {
                // JZ rel8
                let rel = mem.read_u8(paddr + 1) as i8;
                let next = ip.wrapping_add(2);
                if cpu.zf() {
                    let target = (next as i32 + rel as i32) as u16;
                    cpu.set_ip(target);
                } else {
                    cpu.set_ip(next);
                }
            }
            0xB4 => {
                // MOV AH, imm8
                let imm = mem.read_u8(paddr + 1);
                cpu.set_ah(imm);
                cpu.set_ip(ip.wrapping_add(2));
            }
            0xBB => {
                // MOV BX, imm16
                let imm = mem.read_u16(paddr + 1);
                cpu.set_bx(imm);
                cpu.set_ip(ip.wrapping_add(3));
            }
            0xCD => {
                // INT imm8
                let int_no = mem.read_u8(paddr + 1);
                cpu.set_ip(ip.wrapping_add(2));
                bios.handle_interrupt(int_no, cpu, mem, disk, kbd);
            }
            0xEB => {
                // JMP rel8
                let rel = mem.read_u8(paddr + 1) as i8;
                let next = ip.wrapping_add(2);
                let target = (next as i32 + rel as i32) as u16;
                cpu.set_ip(target);
            }
            0xC6 => {
                // MOV r/m8, imm8 (only C6 06 disp16 imm8)
                let modrm = mem.read_u8(paddr + 1);
                assert_eq!(modrm, 0x06);
                let disp = mem.read_u16(paddr + 2);
                let imm = mem.read_u8(paddr + 4);
                let addr = cpu.ds_base().wrapping_add(disp as u32);
                mem.write_u8(addr, imm);
                cpu.set_ip(ip.wrapping_add(5));
            }
            0xF4 => {
                // HLT
                return;
            }
            _ => panic!(
                "unsupported opcode {op:02x} at {paddr:05x} (cs:ip={:04x}:{:04x})",
                cpu.cs, ip
            ),
        }
    }
    panic!("instruction limit reached (possible infinite loop)");
}

fn read_vga_text_line(mem: &SimpleMemory, row: u32, cols: u32) -> String {
    let mut s = String::new();
    let base = 0xB8000u32 + row * 80 * 2;
    for col in 0..cols {
        s.push(mem.read_u8(base + col * 2) as char);
    }
    s
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)) == 0
}

fn read_bytes(mem: &SimpleMemory, paddr: u32, len: usize) -> Vec<u8> {
    (0..len).map(|i| mem.read_u8(paddr + i as u32)).collect()
}

fn scan_region_for_smbios(mem: &SimpleMemory, base: u32, len: u32) -> Option<u32> {
    for off in (0..len).step_by(16) {
        let addr = base + off;
        if mem.read_u8(addr) == b'_'
            && mem.read_u8(addr + 1) == b'S'
            && mem.read_u8(addr + 2) == b'M'
            && mem.read_u8(addr + 3) == b'_'
        {
            return Some(addr);
        }
    }
    None
}

fn find_smbios_eps(mem: &SimpleMemory) -> Option<u32> {
    // SMBIOS spec: search the first KiB of EBDA first, then scan 0xF0000-0xFFFFF
    // on 16-byte boundaries.
    let ebda_seg = mem.read_u16(0x040E);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u32) << 4;
        if let Some(addr) = scan_region_for_smbios(mem, ebda_base, 1024) {
            return Some(addr);
        }
    }
    scan_region_for_smbios(mem, 0xF0000, 0x10000)
}

#[derive(Debug)]
struct ParsedStructure {
    ty: u8,
    formatted: Vec<u8>,
}

fn parse_smbios_table(table: &[u8]) -> Vec<ParsedStructure> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < table.len() {
        let ty = table[i];
        let len = table[i + 1] as usize;
        let formatted = table[i..i + len].to_vec();
        let mut j = i + len;

        // Skip strings.
        loop {
            if j + 1 >= table.len() {
                panic!("unterminated string-set");
            }
            if table[j] == 0 && table[j + 1] == 0 {
                j += 2;
                break;
            }
            j += 1;
        }

        out.push(ParsedStructure { ty, formatted });
        i = j;
        if ty == 127 {
            break;
        }
    }
    out
}

#[test]
fn boots_a_tiny_boot_sector_and_prints_text() {
    let boot = make_test_boot_sector();
    let mut disk = VecDisk::new(boot.to_vec());

    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();

    let mut bios = Bios::new(BiosConfig {
        total_memory_bytes: 64 * 1024 * 1024,
        ..BiosConfig::default()
    });

    bios.post(&mut cpu, &mut mem, &mut disk);

    // BIOS must have loaded the boot sector.
    assert_eq!(&mem.bytes[0x7C00..0x7C00 + 512], &boot);

    // ACPI RSDP should be written during POST when enabled.
    let acpi = bios.acpi_tables().expect("ACPI tables must be present");
    let rsdp = acpi.addresses.rsdp as usize;
    assert_eq!(&mem.bytes[rsdp..rsdp + 8], b"RSD PTR ");

    let mut kbd = NullKeyboard;
    run_real_mode_program(&mut bios, &mut cpu, &mut mem, &mut disk, &mut kbd);

    // Boot sector marker.
    assert_eq!(mem.read_u8(0x0500), 0x42);

    // VGA output: BIOS banner on line 0, then boot sector prints "BOOTED" on line 1.
    assert!(read_vga_text_line(&mem, 0, 9).starts_with("Aero BIOS"));
    assert!(read_vga_text_line(&mem, 1, 6).starts_with("BOOTED"));
}

#[test]
fn bios_post_writes_smbios_tables() {
    let boot = make_test_boot_sector();
    let disk = VecDisk::new(boot.to_vec());

    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();

    let ram_bytes = 96 * 1024 * 1024;
    let mut bios = Bios::new(BiosConfig {
        total_memory_bytes: ram_bytes,
        ..BiosConfig::default()
    });

    bios.post(&mut cpu, &mut mem, &disk);

    let eps_addr = find_smbios_eps(&mem).expect("SMBIOS EPS not found after BIOS POST");
    let eps = read_bytes(&mem, eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(checksum_ok(&eps));
    assert_eq!(&eps[0x10..0x15], b"_DMI_");
    assert!(checksum_ok(&eps[0x10..]));

    let table_len = u16::from_le_bytes([eps[0x16], eps[0x17]]) as usize;
    let table_addr = u32::from_le_bytes([eps[0x18], eps[0x19], eps[0x1A], eps[0x1B]]);
    let table = read_bytes(&mem, table_addr, table_len);
    let structures = parse_smbios_table(&table);

    assert!(structures.iter().any(|s| s.ty == 0), "missing Type 0");
    assert!(structures.iter().any(|s| s.ty == 1), "missing Type 1");
    assert!(structures.iter().any(|s| s.ty == 4), "missing Type 4");
    assert!(structures.iter().any(|s| s.ty == 16), "missing Type 16");
    assert!(structures.iter().any(|s| s.ty == 17), "missing Type 17");
    assert!(structures.iter().any(|s| s.ty == 19), "missing Type 19");
    assert!(structures.iter().any(|s| s.ty == 127), "missing Type 127");

    let type16 = structures
        .iter()
        .find(|s| s.ty == 16)
        .expect("Type 16 missing");
    let max_capacity_kb = u32::from_le_bytes([
        type16.formatted[7],
        type16.formatted[8],
        type16.formatted[9],
        type16.formatted[10],
    ]);
    assert_eq!(u64::from(max_capacity_kb), ram_bytes / 1024);

    let type17 = structures
        .iter()
        .find(|s| s.ty == 17)
        .expect("Type 17 missing");
    let size_mb = u16::from_le_bytes([type17.formatted[12], type17.formatted[13]]);
    assert_eq!(u64::from(size_mb), ram_bytes / (1024 * 1024));

    let type19 = structures
        .iter()
        .find(|s| s.ty == 19)
        .expect("Type 19 missing");
    let start_kb = u32::from_le_bytes([
        type19.formatted[4],
        type19.formatted[5],
        type19.formatted[6],
        type19.formatted[7],
    ]);
    let end_kb = u32::from_le_bytes([
        type19.formatted[8],
        type19.formatted[9],
        type19.formatted[10],
        type19.formatted[11],
    ]);
    assert_eq!(u64::from(start_kb), 0);
    assert_eq!(u64::from(end_kb) + 1, ram_bytes / 1024);
}

#[test]
fn bios_rom_contains_a_valid_reset_vector_jump() {
    let rom = build_bios_rom();
    assert_eq!(rom.len(), 0x10000);
    assert_eq!(&rom[0xFFF0..0xFFF5], &[0xEA, 0x00, 0xE0, 0x00, 0xF0]);
    assert_eq!(&rom[0xFFFE..0x10000], &[0x55, 0xAA]);
}

#[test]
fn int15_e820_returns_a_simple_memory_map() {
    // Disk isn't used by INT 15h, but the firmware API needs one.
    let mut disk = VecDisk::new(vec![0u8; 512]);
    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig {
        total_memory_bytes: 16 * 1024 * 1024,
        ..BiosConfig::default()
    });

    let mut kbd = NullKeyboard;

    cpu.es = 0;
    cpu.edi = 0x0500;

    let mut cont = 0u32;
    let mut entries = Vec::new();
    loop {
        cpu.eax = 0xE820;
        cpu.edx = 0x534D_4150; // 'SMAP'
        cpu.ecx = 24;
        cpu.ebx = cont;
        bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);

        assert!(!cpu.cf());
        assert_eq!(cpu.eax, 0x534D_4150);
        assert_eq!(cpu.ecx, 24);

        let base = mem.read_u32(0x0500) as u64 | ((mem.read_u32(0x0504) as u64) << 32);
        let len = mem.read_u32(0x0508) as u64 | ((mem.read_u32(0x050C) as u64) << 32);
        let typ = mem.read_u32(0x0510);
        entries.push((base, len, typ));

        cont = cpu.ebx;
        if cont == 0 {
            break;
        }
        assert!(entries.len() < 32, "E820 map unexpectedly large");
    }

    // First two entries should match the conventional layout.
    assert_eq!(entries[0], (0, 0x0009_F000, 1));
    assert_eq!(entries[1], (0x0009_F000, 0x0006_1000, 2));

    // We should expose an ACPI reclaimable region (type 3) for the firmware tables.
    assert!(
        entries
            .iter()
            .any(|&(base, len, typ)| typ == 3 && base >= 0x0010_0000 && len > 0),
        "expected an ACPI E820 entry (type=3) above 1MiB, got: {entries:?}"
    );

    // There should still be usable RAM above 1MiB for the guest.
    assert!(
        entries
            .iter()
            .any(|&(base, len, typ)| typ == 1 && base >= 0x0010_0000 && len > 0),
        "expected a RAM E820 entry (type=1) above 1MiB, got: {entries:?}"
    );
}

#[test]
fn int13_extended_and_chs_reads_copy_sectors_into_memory() {
    // Build a 3-sector disk: [MBR][sector1][sector2].
    let mut bytes = vec![0u8; 3 * 512];
    bytes[510] = 0x55;
    bytes[511] = 0xAA;
    bytes[1 * 512..2 * 512].fill(0xA5);
    bytes[2 * 512..3 * 512].fill(0x5A);
    let mut disk = VecDisk::new(bytes);

    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig::default());
    let mut kbd = NullKeyboard;

    // INT 13h AH=42h extended read of LBA 1 -> 0x0800.
    mem.write_u8(0x0600, 0x10); // size
    mem.write_u8(0x0601, 0); // reserved
    mem.write_u16(0x0602, 1); // sectors
    mem.write_u16(0x0604, 0x0800); // buffer offset
    mem.write_u16(0x0606, 0x0000); // buffer segment
    mem.write_u32(0x0608, 1); // lba low
    mem.write_u32(0x060C, 0); // lba high

    cpu.ds = 0;
    cpu.esi = 0x0600;
    cpu.set_ah(0x42);
    cpu.set_dl(0x80);
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(&mem.bytes[0x0800..0x0800 + 512], vec![0xA5; 512]);

    // INT 13h AH=02h CHS read of sector=2 (LBA 1) -> 0x0900.
    cpu.set_ah(0x02);
    cpu.set_al(1); // count
    cpu.es = 0;
    cpu.set_bx(0x0900);
    cpu.set_cx(0x0002); // CH=0, CL=2
    cpu.set_dh(0); // head 0
    cpu.set_dl(0x80);
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(&mem.bytes[0x0900..0x0900 + 512], vec![0xA5; 512]);
}

#[test]
fn int13_chs_and_extended_writes_persist_to_disk() {
    // 3-sector disk: [MBR][sector1][sector2]
    let mut bytes = vec![0u8; 3 * 512];
    bytes[510] = 0x55;
    bytes[511] = 0xAA;
    let mut disk = VecDisk::new(bytes);

    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig::default());
    let mut kbd = NullKeyboard;

    // CHS write: write LBA1 (CH=0, DH=0, CL=2) from 0x0800.
    mem.bytes[0x0800..0x0800 + 512].fill(0xCC);
    cpu.set_ah(0x03);
    cpu.set_al(1);
    cpu.es = 0;
    cpu.set_bx(0x0800);
    cpu.set_cx(0x0002);
    cpu.set_dh(0);
    cpu.set_dl(0x80);
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());

    let mut sector = [0u8; 512];
    disk.read_sector(1, &mut sector).unwrap();
    assert_eq!(sector, [0xCCu8; 512]);

    // Extended write (DAP) for LBA2 from 0x0900.
    mem.bytes[0x0900..0x0900 + 512].fill(0xDD);
    mem.write_u8(0x0600, 0x10); // size
    mem.write_u8(0x0601, 0); // reserved
    mem.write_u16(0x0602, 1); // sectors
    mem.write_u16(0x0604, 0x0900); // buffer offset
    mem.write_u16(0x0606, 0x0000); // buffer segment
    mem.write_u32(0x0608, 2); // lba low
    mem.write_u32(0x060C, 0); // lba high

    cpu.ds = 0;
    cpu.esi = 0x0600;
    cpu.set_ah(0x43);
    cpu.set_dl(0x80);
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());

    disk.read_sector(2, &mut sector).unwrap();
    assert_eq!(sector, [0xDDu8; 512]);
}

#[test]
fn int11_and_int12_reflect_bda_equipment_and_conventional_memory() {
    let mut boot = [0u8; 512];
    boot[510] = 0x55;
    boot[511] = 0xAA;
    let mut disk = VecDisk::new(boot.to_vec());

    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig::default());
    let mut kbd = NullKeyboard;

    bios.post(&mut cpu, &mut mem, &mut disk);

    bios.handle_interrupt(0x11, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert_eq!(cpu.ax(), mem.read_u16(0x0410));

    bios.handle_interrupt(0x12, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert_eq!(cpu.ax(), mem.read_u16(0x0413));
}

#[test]
fn int1a_time_services_return_bcd_values() {
    let mut disk = VecDisk::new(vec![0u8; 512]);
    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig::default());
    let mut kbd = NullKeyboard;

    // AH=00h: ticks since midnight.
    cpu.set_ah(0x00);
    bios.handle_interrupt(0x1A, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    let ticks = ((cpu.cx() as u32) << 16) | cpu.dx() as u32;
    assert!(ticks < 1_573_040);

    // AH=02h: RTC time (BCD).
    cpu.set_ah(0x02);
    bios.handle_interrupt(0x1A, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    let hour = (cpu.cx() >> 8) as u8;
    let min = cpu.cx() as u8;
    let sec = (cpu.dx() >> 8) as u8;

    fn bcd_to_u8(b: u8) -> u8 {
        ((b >> 4) & 0x0F) * 10 + (b & 0x0F)
    }

    let h = bcd_to_u8(hour);
    let m = bcd_to_u8(min);
    let s = bcd_to_u8(sec);
    assert!(h < 24);
    assert!(m < 60);
    assert!(s < 60);

    // AH=04h: RTC date (BCD). Ensure digits decode to a plausible range.
    cpu.set_ah(0x04);
    bios.handle_interrupt(0x1A, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    let century = bcd_to_u8((cpu.cx() >> 8) as u8);
    let year = bcd_to_u8(cpu.cx() as u8);
    let month = bcd_to_u8((cpu.dx() >> 8) as u8);
    let day = bcd_to_u8(cpu.dx() as u8);
    assert!(century >= 19);
    assert!(year <= 99);
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
}

#[test]
fn int15_e801_reports_extended_memory_sizing() {
    let mut disk = VecDisk::new(vec![0u8; 512]);
    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig {
        total_memory_bytes: 64 * 1024 * 1024,
        ..BiosConfig::default()
    });
    let mut kbd = NullKeyboard;

    cpu.set_ax(0xE801);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());

    // 1MiB..16MiB = 15MiB = 15360 KiB.
    assert_eq!(cpu.ax(), 0x3C00);
    assert_eq!(cpu.cx(), 0x3C00);

    // Remaining 48MiB above 16MiB => 48MiB / 64KiB = 768 blocks.
    assert_eq!(cpu.bx(), 0x0300);
    assert_eq!(cpu.dx(), 0x0300);
}

#[test]
fn int15_a20_enable_disable_query_roundtrips() {
    let mut disk = VecDisk::new(vec![0u8; 512]);
    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig::default());
    let mut kbd = NullKeyboard;

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.al(), 1);

    cpu.set_ax(0x2400);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.al(), 0);

    cpu.set_ax(0x2401);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.al(), 1);

    cpu.set_ax(0x2403);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.bx(), 0x0003);
}

#[test]
fn int15_e820_includes_pci_hole_for_large_memory() {
    let mut disk = VecDisk::new(vec![0u8; 512]);
    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig {
        total_memory_bytes: 4 * 1024 * 1024 * 1024,
        ..BiosConfig::default()
    });
    let mut kbd = NullKeyboard;

    cpu.es = 0;
    cpu.edi = 0x0500;

    let mut cont = 0u32;
    let mut entries = Vec::new();
    loop {
        cpu.eax = 0xE820;
        cpu.edx = 0x534D_4150;
        cpu.ecx = 24;
        cpu.ebx = cont;
        bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);

        assert!(!cpu.cf());
        let base = mem.read_u32(0x0500) as u64 | ((mem.read_u32(0x0504) as u64) << 32);
        let len = mem.read_u32(0x0508) as u64 | ((mem.read_u32(0x050C) as u64) << 32);
        let typ = mem.read_u32(0x0510);
        entries.push((base, len, typ));

        cont = cpu.ebx;
        if cont == 0 {
            break;
        }
        assert!(entries.len() < 64, "E820 map unexpectedly large");
    }

    assert!(
        entries
            .iter()
            .any(|&(base, len, typ)| base == 0xC000_0000 && len == 0x4000_0000 && typ == 2),
        "expected a PCI hole reserved entry, got: {entries:?}"
    );

    assert!(
        entries
            .iter()
            .any(|&(base, len, typ)| base == 0x1_0000_0000 && len > 0 && typ == 1),
        "expected a high RAM entry above 4GiB, got: {entries:?}"
    );
}
