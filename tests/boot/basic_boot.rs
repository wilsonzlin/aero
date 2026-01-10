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
    fn read_sector(&self, lba: u64, buf512: &mut [u8; 512]) -> Result<(), DiskError> {
        let start = lba
            .checked_mul(512)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let slice = self.bytes.get(start..end).ok_or(DiskError::OutOfRange)?;
        buf512.copy_from_slice(slice);
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
    disk: &VecDisk,
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

#[test]
fn boots_a_tiny_boot_sector_and_prints_text() {
    let boot = make_test_boot_sector();
    let disk = VecDisk::new(boot.to_vec());

    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();

    let mut bios = Bios::new(BiosConfig {
        total_memory_bytes: 64 * 1024 * 1024,
        ..BiosConfig::default()
    });

    bios.post(&mut cpu, &mut mem, &disk);

    // BIOS must have loaded the boot sector.
    assert_eq!(&mem.bytes[0x7C00..0x7C00 + 512], &boot);

    let mut kbd = NullKeyboard;
    run_real_mode_program(&mut bios, &mut cpu, &mut mem, &disk, &mut kbd);

    // Boot sector marker.
    assert_eq!(mem.read_u8(0x0500), 0x42);

    // VGA output: BIOS banner on line 0, then boot sector prints "BOOTED" on line 1.
    assert!(read_vga_text_line(&mem, 0, 9).starts_with("Aero BIOS"));
    assert!(read_vga_text_line(&mem, 1, 6).starts_with("BOOTED"));
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
    let disk = VecDisk::new(vec![0u8; 512]);
    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut bios = Bios::new(BiosConfig {
        total_memory_bytes: 16 * 1024 * 1024,
        ..BiosConfig::default()
    });

    cpu.es = 0;
    cpu.edi = 0x0500;
    cpu.eax = 0xE820;
    cpu.edx = 0x534D_4150; // 'SMAP'
    cpu.ecx = 24;
    cpu.ebx = 0;

    let mut kbd = NullKeyboard;
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &disk, &mut kbd);

    assert!(!cpu.cf());
    assert_eq!(cpu.eax, 0x534D_4150);
    assert_eq!(cpu.ecx, 24);
    assert_eq!(cpu.ebx, 1);

    let base0 = mem.read_u32(0x0500) as u64 | ((mem.read_u32(0x0504) as u64) << 32);
    let len0 = mem.read_u32(0x0508) as u64 | ((mem.read_u32(0x050C) as u64) << 32);
    let typ0 = mem.read_u32(0x0510);
    assert_eq!(base0, 0);
    assert_eq!(len0, 0x0009_F000);
    assert_eq!(typ0, 1);

    // Next entry.
    cpu.eax = 0xE820;
    cpu.edx = 0x534D_4150;
    cpu.ecx = 24;
    // cpu.ebx already contains the continuation.
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &disk, &mut kbd);

    assert!(!cpu.cf());
    assert_eq!(cpu.ebx, 2);
    let base1 = mem.read_u32(0x0500) as u64 | ((mem.read_u32(0x0504) as u64) << 32);
    let len1 = mem.read_u32(0x0508) as u64 | ((mem.read_u32(0x050C) as u64) << 32);
    let typ1 = mem.read_u32(0x0510);
    assert_eq!(base1, 0x0009_F000);
    assert_eq!(len1, 0x0006_1000);
    assert_eq!(typ1, 2);

    // Final entry should set EBX=0.
    cpu.eax = 0xE820;
    cpu.edx = 0x534D_4150;
    cpu.ecx = 24;
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.ebx, 0);
}

#[test]
fn int13_extended_and_chs_reads_copy_sectors_into_memory() {
    // Build a 3-sector disk: [MBR][sector1][sector2].
    let mut bytes = vec![0u8; 3 * 512];
    bytes[510] = 0x55;
    bytes[511] = 0xAA;
    bytes[1 * 512..2 * 512].fill(0xA5);
    bytes[2 * 512..3 * 512].fill(0x5A);
    let disk = VecDisk::new(bytes);

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
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &disk, &mut kbd);
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
    bios.handle_interrupt(0x13, &mut cpu, &mut mem, &disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(&mem.bytes[0x0900..0x0900 + 512], vec![0xA5; 512]);
}
