use aero_cpu::tier0::{CpuMode, CpuState, EmuException, Flag, Machine, MemoryBus, PortIo, StepOutcome};
use std::collections::HashMap;

struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn load(&mut self, addr: u64, bytes: &[u8]) {
        self.data[addr as usize..addr as usize + bytes.len()].copy_from_slice(bytes);
    }
}

impl MemoryBus for TestMem {
    fn read_u8(&mut self, paddr: u64) -> Result<u8, EmuException> {
        self.data
            .get(paddr as usize)
            .copied()
            .ok_or(EmuException::MemOutOfBounds(paddr))
    }

    fn write_u8(&mut self, paddr: u64, value: u8) -> Result<(), EmuException> {
        let Some(slot) = self.data.get_mut(paddr as usize) else {
            return Err(EmuException::MemOutOfBounds(paddr));
        };
        *slot = value;
        Ok(())
    }
}

#[derive(Default)]
struct TestPorts {
    debugcon: Vec<u8>,
    in_u8: HashMap<u16, u8>,
    out_u8: Vec<(u16, u8)>,
}

impl PortIo for TestPorts {
    fn in_u8(&mut self, _port: u16) -> u8 {
        *self.in_u8.get(&_port).unwrap_or(&0)
    }

    fn out_u8(&mut self, port: u16, value: u8) {
        self.out_u8.push((port, value));
        if port == 0xE9 {
            self.debugcon.push(value);
        }
    }
}

fn run_test(
    mode: CpuMode,
    code: &[u8],
    init: impl FnOnce(&mut CpuState, &mut TestMem, &mut TestPorts),
) -> Machine<TestMem, TestPorts> {
    let mut mem = TestMem::new(1024 * 1024);
    mem.load(0, code);
    let mut cpu = CpuState::new(mode);
    cpu.set_segment_selector(iced_x86::Register::CS, 0).unwrap();
    cpu.set_segment_selector(iced_x86::Register::DS, 0).unwrap();
    cpu.set_segment_selector(iced_x86::Register::ES, 0).unwrap();
    cpu.set_segment_selector(iced_x86::Register::SS, 0).unwrap();
    cpu.set_rip(0);
    let mut ports = TestPorts::default();
    init(&mut cpu, &mut mem, &mut ports);

    let mut machine = Machine::new(cpu, mem, ports);
    machine.run(10_000).unwrap();
    machine
}

fn make_machine(
    mode: CpuMode,
    code: &[u8],
    init: impl FnOnce(&mut CpuState, &mut TestMem, &mut TestPorts),
) -> Machine<TestMem, TestPorts> {
    let mut mem = TestMem::new(1024 * 1024);
    mem.load(0, code);
    let mut cpu = CpuState::new(mode);
    cpu.set_segment_selector(iced_x86::Register::CS, 0).unwrap();
    cpu.set_segment_selector(iced_x86::Register::DS, 0).unwrap();
    cpu.set_segment_selector(iced_x86::Register::ES, 0).unwrap();
    cpu.set_segment_selector(iced_x86::Register::SS, 0).unwrap();
    cpu.set_rip(0);
    let mut ports = TestPorts::default();
    init(&mut cpu, &mut mem, &mut ports);
    Machine::new(cpu, mem, ports)
}

#[test]
fn boot_sector_hello_via_int10() {
    // A tiny boot sector that prints "Hello" using INT 10h.
    // We install an IVT handler for INT 10h that outputs AL to port 0xE9 and IRET's.
    let mut mem = TestMem::new(1024 * 1024);

    let boot_addr = 0x7C00u64;
    let msg_off = 0x7C00u16 + 0x1B; // after code below
    let boot = [
        0x31,
        0xC0, // xor ax,ax
        0x8E,
        0xD8, // mov ds,ax
        0x8E,
        0xC0, // mov es,ax
        0x8E,
        0xD0, // mov ss,ax
        0xBC,
        0x00,
        0x7C, // mov sp,0x7c00
        0xBE,
        (msg_off & 0xFF) as u8,
        (msg_off >> 8) as u8, // mov si,msg
        0xFC,                 // cld
        0xAC,                 // lodsb
        0x0A,
        0xC0, // or al,al
        0x74,
        0x06, // jz done
        0xB4,
        0x0E, // mov ah,0x0e
        0xCD,
        0x10, // int 0x10
        0xEB,
        0xF5, // jmp loop (back 11 bytes)
        0xF4, // done: hlt
        b'H',
        b'e',
        b'l',
        b'l',
        b'o',
        0,
    ];
    mem.load(boot_addr, &boot);

    // BIOS handler at F000:0100 => physical 0xF0100.
    let bios_seg = 0xF000u16;
    let bios_off = 0x0100u16;
    let bios_addr = ((bios_seg as u64) << 4) + bios_off as u64;
    let bios = [
        0x50, // push ax
        0xE6, 0xE9, // out 0xE9, al
        0x58, // pop ax
        0xCF, // iret
    ];
    mem.load(bios_addr, &bios);

    // IVT[0x10] = bios handler.
    let ivt = 0x10u64 * 4;
    mem.write_u16(ivt, bios_off).unwrap();
    mem.write_u16(ivt + 2, bios_seg).unwrap();

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_segment_selector(iced_x86::Register::CS, 0).unwrap();
    cpu.set_rip(boot_addr);

    let ports = TestPorts::default();
    let mut machine = Machine::new(cpu, mem, ports);
    machine.run(10_000).unwrap();

    assert_eq!(machine.step().unwrap(), StepOutcome::Halted);
    assert_eq!(
        std::str::from_utf8(&machine.ports.debugcon).unwrap(),
        "Hello"
    );
}

#[test]
fn mov_lea_xchg() {
    // mov ax,0x5678; mov bx,0x1234; lea di,[bx+si+0x10]; xchg ax,bx
    let code = [
        0xB8, 0x78, 0x56, // mov ax,0x5678
        0xBB, 0x34, 0x12, // mov bx,0x1234
        0xBE, 0x04, 0x00, // mov si,0x0004
        0x8D, 0x78, 0x10, // lea di,[bx+si+0x10]
        0x93, // xchg ax,bx
        0xF4, // hlt
    ];

    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });

    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::AX).unwrap(),
        0x1234
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::BX).unwrap(),
        0x5678
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::DI).unwrap(),
        0x1234 + 0x0004 + 0x10
    );
}

#[test]
fn movzx_movsx() {
    // mov al,0x80; movsx ecx,al; movzx edx,al
    let code = [
        0xB0, 0x80, // mov al,0x80
        0x0F, 0xBE, 0xC8, // movsx ecx,al
        0x0F, 0xB6, 0xD0, // movzx edx,al
        0xF4, // hlt
    ];

    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x8000).unwrap();
    });

    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::ECX).unwrap(),
        0xFFFF_FF80
    );
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EDX).unwrap(), 0x80);
}

#[test]
fn push_pop_and_pusha_popa() {
    let code = [
        0xB8, 0x34, 0x12, // mov ax,0x1234
        0x50, // push ax
        0xB8, 0x00, 0x00, // mov ax,0
        0x5B, // pop bx
        0x60, // pusha
        0xB8, 0x00, 0x00, // mov ax,0
        0x61, // popa
        0xF4, // hlt
    ];

    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x0100).unwrap();
        cpu.write_reg(iced_x86::Register::CX, 0x2222).unwrap();
        cpu.write_reg(iced_x86::Register::DX, 0x3333).unwrap();
        cpu.write_reg(iced_x86::Register::BX, 0x4444).unwrap();
        cpu.write_reg(iced_x86::Register::BP, 0x5555).unwrap();
        cpu.write_reg(iced_x86::Register::SI, 0x6666).unwrap();
        cpu.write_reg(iced_x86::Register::DI, 0x7777).unwrap();
    });

    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::BX).unwrap(),
        0x1234
    );
    // AX was 0 when PUSHA executed, so POPA restores it back to 0.
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::AX).unwrap(),
        0x0000
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::SP).unwrap(),
        0x0100
    );
}

#[test]
fn add_flags() {
    let code = [0x00, 0xD8, 0xF4]; // add al,bl; hlt
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 0xFF).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 0x01).unwrap();
    });

    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0x00);
    assert!(machine.cpu.get_flag(Flag::Cf));
    assert!(machine.cpu.get_flag(Flag::Zf));
    assert!(machine.cpu.get_flag(Flag::Af));
    assert!(!machine.cpu.get_flag(Flag::Of));
    assert!(!machine.cpu.get_flag(Flag::Sf));
}

#[test]
fn adc_sbb_and_cmp_flags() {
    // adc al,bl; cmp al,bl; sbb al,bl
    let adc = [0x10, 0xD8, 0xF4]; // adc al,bl
    let machine = run_test(CpuMode::Real, &adc, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 1).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 1).unwrap();
        cpu.set_flag(Flag::Cf, true);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 3);
    assert!(!machine.cpu.get_flag(Flag::Cf));

    let cmp = [0x38, 0xD8, 0xF4]; // cmp al,bl
    let machine = run_test(CpuMode::Real, &cmp, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 1).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 2).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 1);
    assert!(machine.cpu.get_flag(Flag::Cf));

    let sbb = [0x18, 0xD8, 0xF4]; // sbb al,bl
    let machine = run_test(CpuMode::Real, &sbb, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 0).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 0).unwrap();
        cpu.set_flag(Flag::Cf, true);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0xFF);
    assert!(machine.cpu.get_flag(Flag::Cf));
}

#[test]
fn inc_dec_preserve_cf() {
    let code = [
        0xFE, 0xC0, // inc al
        0xFE, 0xC8, // dec al
        0xF4,
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 0x7F).unwrap();
        cpu.set_flag(Flag::Cf, true);
    });

    assert!(machine.cpu.get_flag(Flag::Cf));
    assert!(machine.cpu.get_flag(Flag::Of)); // inc 0x7F => 0x80 overflows
}

#[test]
fn mul_div_idiv_and_divide_error() {
    let mul = [0xF6, 0xE3, 0xF4]; // mul bl
    let machine = run_test(CpuMode::Real, &mul, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 0x10).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 0x10).unwrap();
    });
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::AX).unwrap(),
        0x0100
    );
    assert!(machine.cpu.get_flag(Flag::Cf));
    assert!(machine.cpu.get_flag(Flag::Of));

    let div = [0xF6, 0xF3, 0xF4]; // div bl
    let machine = run_test(CpuMode::Real, &div, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AX, 0x0100).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 0x10).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0x10);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AH).unwrap(), 0x00);

    let idiv = [0xF6, 0xFB, 0xF4]; // idiv bl
    let machine = run_test(CpuMode::Real, &idiv, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AX, 0xFFF0).unwrap(); // -16
        cpu.write_reg(iced_x86::Register::BL, 0xF0).unwrap(); // -16
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 1);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AH).unwrap(), 0);

    // Divide by zero -> #DE
    let mut machine = make_machine(CpuMode::Real, &[0xF6, 0xF3, 0xF4], |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AX, 1).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 0).unwrap();
    });
    let err = machine.run(10).unwrap_err();
    assert_eq!(err, EmuException::DivideError);
}

#[test]
fn logic_and_shift_rotate() {
    let and_ = [0x20, 0xD8, 0xF4]; // and al,bl
    let machine = run_test(CpuMode::Real, &and_, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 0xF0).unwrap();
        cpu.write_reg(iced_x86::Register::BL, 0x0F).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0);
    assert!(machine.cpu.get_flag(Flag::Zf));
    assert!(!machine.cpu.get_flag(Flag::Cf));
    assert!(!machine.cpu.get_flag(Flag::Of));

    let neg = [0xF6, 0xD8, 0xF4]; // neg al
    let machine = run_test(CpuMode::Real, &neg, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 1).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0xFF);
    assert!(machine.cpu.get_flag(Flag::Cf));

    let shl = [0xD0, 0xE0, 0xF4]; // shl al,1
    let machine = run_test(CpuMode::Real, &shl, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 0x81).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0x02);
    assert!(machine.cpu.get_flag(Flag::Cf));
    assert!(machine.cpu.get_flag(Flag::Of));

    let ror = [0xD0, 0xC8, 0xF4]; // ror al,1
    let machine = run_test(CpuMode::Real, &ror, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        cpu.write_reg(iced_x86::Register::AL, 0x81).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0xC0);
    assert!(machine.cpu.get_flag(Flag::Cf));
}

#[test]
fn cmovcc_and_setcc() {
    // cmp eax,ebx sets CF=1; cmovb and setb should observe it.
    let code = [
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xBB, 0x02, 0x00, 0x00, 0x00, // mov ebx,2
        0xB9, 0x11, 0x11, 0x11, 0x11, // mov ecx,0x11111111
        0xBA, 0x22, 0x22, 0x22, 0x22, // mov edx,0x22222222
        0x39, 0xD8, // cmp eax,ebx
        0x0F, 0x42, 0xCA, // cmovb ecx,edx
        0x0F, 0x92, 0xC0, // setb al
        0xF4, // hlt
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
    });
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::ECX).unwrap(),
        0x2222_2222
    );
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 1);
    assert!(machine.cpu.get_flag(Flag::Cf));
}

#[test]
fn pushf_popf_and_clc_stc_cmc() {
    // stc; pushf; clc; popf (restores CF=1); cmc (toggles to 0)
    let code = [0xF9, 0x9C, 0xF8, 0x9D, 0xF5, 0xF4];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x0200).unwrap();
    });
    assert!(!machine.cpu.get_flag(Flag::Cf));
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::SP).unwrap(), 0x0200);
}

#[test]
fn xadd_and_bswap() {
    // xadd al,bl; bswap ecx
    let code = [
        0xB0, 0x01, // mov al,1
        0xB3, 0x02, // mov bl,2
        0x0F, 0xC0, 0xD8, // xadd al,bl
        0xB9, 0x78, 0x56, 0x34, 0x12, // mov ecx,0x12345678
        0x0F, 0xC9, // bswap ecx
        0xF4,
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 3);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::BL).unwrap(), 1);
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::ECX).unwrap(),
        0x7856_3412
    );
}

#[test]
fn bit_ops_bt_bts_btr_btc() {
    let code = [
        0xB8, 0x00, 0x00, // mov ax,0
        0x0F, 0xBA, 0xE8, 0x02, // bts ax,2
        0x0F, 0xBA, 0xE8, 0x02, // bts ax,2
        0x0F, 0xBA, 0xF0, 0x02, // btr ax,2
        0x0F, 0xBA, 0xF8, 0x01, // btc ax,1
        0x0F, 0xBA, 0xF8, 0x01, // btc ax,1
        0xF4, // hlt
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX).unwrap(), 0);
    assert!(machine.cpu.get_flag(Flag::Cf));
}

#[test]
fn bsf_and_bsr() {
    let code = [
        0xB8, 0x10, 0x00, 0x00, 0x00, // mov eax,0x10
        0x0F, 0xBC, 0xC8, // bsf ecx,eax
        0x89, 0xCB, // mov ebx,ecx
        0x0F, 0xBD, 0xD0, // bsr edx,eax
        0x31, 0xC0, // xor eax,eax
        0xB9, 0x78, 0x56, 0x34, 0x12, // mov ecx,0x12345678
        0x0F, 0xBC, 0xC8, // bsf ecx,eax (src==0)
        0xF4,
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EBX).unwrap(), 4);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EDX).unwrap(), 4);
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::ECX).unwrap(),
        0x1234_5678
    );
    assert!(machine.cpu.get_flag(Flag::Zf));
}

#[test]
fn rcl_rcr_shld_shrd() {
    let code = [
        0xF9, // stc
        0xB0, 0x80, // mov al,0x80
        0xD0, 0xD0, // rcl al,1
        0xD0, 0xD8, // rcr al,1
        0x89, 0xC6, // mov si,ax
        0xB8, 0x34, 0x12, // mov ax,0x1234
        0xBB, 0xCD, 0xAB, // mov bx,0xABCD
        0x0F, 0xA4, 0xD8, 0x04, // shld ax,bx,4
        0x89, 0xC1, // mov cx,ax
        0xBA, 0x34, 0x12, // mov dx,0x1234
        0x0F, 0xAC, 0xDA, 0x04, // shrd dx,bx,4
        0xF4,
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::SI).unwrap(), 0x80);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::CX).unwrap(), 0x234A);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::DX).unwrap(), 0xD123);
}

#[test]
fn jecxz() {
    let code = [
        0xB9, 0x00, 0x00, 0x00, 0x00, // mov ecx,0
        0xE3, 0x05, // jecxz +5
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xF4,
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EAX).unwrap(), 0);
}

#[test]
fn cbw_cwd_cwde_cdq_cdqe_cqo() {
    let real = [0xB0, 0x80, 0x98, 0x99, 0xF4]; // mov al,0x80; cbw; cwd; hlt
    let machine = run_test(CpuMode::Real, &real, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX).unwrap(), 0xFF80);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::DX).unwrap(), 0xFFFF);

    let prot = [0x66, 0xB8, 0x00, 0x80, 0x98, 0x99, 0xF4]; // mov ax,0x8000; cwde; cdq; hlt
    let machine = run_test(CpuMode::Protected, &prot, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
    });
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::EAX).unwrap(),
        0xFFFF_8000
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::EDX).unwrap(),
        0xFFFF_FFFF
    );

    let long = [0xB8, 0x00, 0x00, 0x00, 0x80, 0x48, 0x98, 0x48, 0x99, 0xF4]; // mov eax,0x80000000; cdqe; cqo; hlt
    let machine = run_test(CpuMode::Long, &long, |_cpu, _mem, _ports| {});
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::RAX).unwrap(),
        0xFFFF_FFFF_8000_0000
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::RDX).unwrap(),
        0xFFFF_FFFF_FFFF_FFFF
    );
}

#[test]
fn control_flow() {
    // jmp short skips mov ax,1.
    let jmp = [0xEB, 0x03, 0xB8, 0x01, 0x00, 0x31, 0xC0, 0xF4];
    let machine = run_test(CpuMode::Real, &jmp, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX).unwrap(), 0);

    // near call/ret
    let call = [
        0xB8, 0x00, 0x00, 0xE8, 0x01, 0x00, 0xF4, 0xB8, 0x34, 0x12, 0xC3,
    ];
    let machine = run_test(CpuMode::Real, &call, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::AX).unwrap(),
        0x1234
    );

    // far call/retf
    let far = [
        0x9A, 0x08, 0x00, 0x00, 0x00, // call far 0000:0008
        0xF4, // hlt
        0x90, 0x90, // padding
        0xB8, 0x34, 0x12, // mov ax,0x1234
        0xCB, // retf
    ];
    let machine = run_test(CpuMode::Real, &far, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::AX).unwrap(),
        0x1234
    );

    // loop
    let loop_ = [0xB9, 0x03, 0x00, 0x31, 0xC0, 0x40, 0xE2, 0xFD, 0xF4];
    let machine = run_test(CpuMode::Real, &loop_, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX).unwrap(), 3);

    // jcxz
    let jcxz = [0xB9, 0x00, 0x00, 0xE3, 0x03, 0xB8, 0x01, 0x00, 0xF4];
    let machine = run_test(CpuMode::Real, &jcxz, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX).unwrap(), 0);
}

#[test]
fn string_ops_rep_movs_stos_lods_cmps_scas() {
    let src = 0x0100u16;
    let dst = 0x0200u16;
    let code = [
        0xFC, // cld
        0xBE,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov si,src
        0xBF,
        (dst & 0xFF) as u8,
        (dst >> 8) as u8, // mov di,dst
        0xB9,
        0x04,
        0x00, // mov cx,4
        0xF3,
        0xA4, // rep movsb
        0xBF,
        (dst & 0xFF) as u8,
        (dst >> 8) as u8, // mov di,dst
        0xB0,
        0xAA, // mov al,0xAA
        0xB9,
        0x04,
        0x00, // mov cx,4
        0xF3,
        0xAA, // rep stosb
        0xBE,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov si,src
        0xAC,             // lodsb
        0xBE,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov si,src
        0xBF,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov di,src
        0xA6,             // cmpsb (equal)
        0xBF,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov di,src
        0xAE,             // scasb (equal)
        0xF4,
    ];

    let machine = run_test(CpuMode::Real, &code, |_cpu, mem, _ports| {
        mem.load(src as u64, &[1, 2, 3, 4]);
        mem.load(dst as u64, &[1, 2, 3, 4]);
    });

    assert_eq!(
        &machine.mem.data[dst as usize..dst as usize + 4],
        &[0xAA; 4]
    );
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 1);
    assert!(machine.cpu.get_flag(Flag::Zf));
}

#[test]
fn system_in_out_cli_sti_cpuid_rdtsc_rdmsr_wrmsr_lgdt_lidt_ltr() {
    // in/out + cli/sti
    let in_out = [0xFB, 0xFA, 0xE4, 0x10, 0xE6, 0x11, 0xF4];
    let machine = run_test(CpuMode::Real, &in_out, |cpu, _mem, ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        ports.in_u8.insert(0x10, 0xAB);
    });
    assert!(!machine.cpu.get_flag(Flag::If));
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL).unwrap(), 0xAB);
    assert!(machine.ports.out_u8.contains(&(0x11, 0xAB)));

    // cpuid vendor string (leaf 0)
    let cpuid = [0x0F, 0xA2, 0xF4];
    let machine = run_test(CpuMode::Protected, &cpuid, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
        cpu.write_reg(iced_x86::Register::EAX, 0).unwrap();
        cpu.write_reg(iced_x86::Register::ECX, 0).unwrap();
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EAX).unwrap(), 1);

    // rdtsc deterministic readback
    let rdtsc = [0x0F, 0x31, 0xF4];
    let machine = run_test(CpuMode::Protected, &rdtsc, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
        cpu.tsc = 0x1234_5678_9ABC_DEF0;
    });
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::EAX).unwrap(),
        0x9ABC_DEF0
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::EDX).unwrap(),
        0x1234_5678
    );

    // wrmsr/rdmsr
    let msr = [0x0F, 0x30, 0x0F, 0x32, 0xF4];
    let machine = run_test(CpuMode::Protected, &msr, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000).unwrap();
        cpu.write_reg(iced_x86::Register::ECX, 0x10).unwrap();
        cpu.write_reg(iced_x86::Register::EAX, 0x1234_5678).unwrap();
        cpu.write_reg(iced_x86::Register::EDX, 0x9ABC_DEF0).unwrap();
    });
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::EAX).unwrap(),
        0x1234_5678
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::EDX).unwrap(),
        0x9ABC_DEF0
    );

    // lgdt/lidt/ltr
    let ldt = [
        0x0F, 0x01, 0x16, 0x00, 0x03, // lgdt [0x0300]
        0x0F, 0x01, 0x1E, 0x06, 0x03, // lidt [0x0306]
        0xB8, 0x28, 0x00, // mov ax,0x28
        0x0F, 0x00, 0xD8, // ltr ax
        0xF4,
    ];
    let machine = run_test(CpuMode::Real, &ldt, |cpu, mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
        mem.write_u16(0x0300, 0x0017).unwrap();
        mem.write_u32(0x0302, 0x1122_3344).unwrap();
        mem.write_u16(0x0306, 0x00FF).unwrap();
        mem.write_u32(0x0308, 0x5566_7788).unwrap();
    });
    assert_eq!(machine.cpu.tr, 0x28);
    assert_eq!(machine.cpu.gdtr.limit, 0x0017);
    assert_eq!(machine.cpu.gdtr.base, 0x1122_3344);
    assert_eq!(machine.cpu.idtr.limit, 0x00FF);
    assert_eq!(machine.cpu.idtr.base, 0x5566_7788);
}

#[test]
fn mov_to_cr0_pe_switches_to_protected_mode() {
    let code = [
        0x66, 0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x66, 0x0F, 0x22, 0xC0, // mov cr0,eax
        0xF4, // hlt
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200).unwrap();
    });
    assert_eq!(machine.cpu.mode, CpuMode::Protected);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RealRegs16 {
    ax: u16,
    bx: u16,
    cx: u16,
    dx: u16,
    sp: u16,
    bp: u16,
    si: u16,
    di: u16,
    flags: u16,
}

fn run_aero_real(test: &[u8], init: RealRegs16) -> RealRegs16 {
    let mut code = Vec::from(test);
    code.push(0xF4); // hlt
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::AX, init.ax as u64)
            .unwrap();
        cpu.write_reg(iced_x86::Register::BX, init.bx as u64)
            .unwrap();
        cpu.write_reg(iced_x86::Register::CX, init.cx as u64)
            .unwrap();
        cpu.write_reg(iced_x86::Register::DX, init.dx as u64)
            .unwrap();
        cpu.write_reg(iced_x86::Register::SP, init.sp as u64)
            .unwrap();
        cpu.write_reg(iced_x86::Register::BP, init.bp as u64)
            .unwrap();
        cpu.write_reg(iced_x86::Register::SI, init.si as u64)
            .unwrap();
        cpu.write_reg(iced_x86::Register::DI, init.di as u64)
            .unwrap();
    });
    RealRegs16 {
        ax: machine.cpu.read_reg(iced_x86::Register::AX).unwrap() as u16,
        bx: machine.cpu.read_reg(iced_x86::Register::BX).unwrap() as u16,
        cx: machine.cpu.read_reg(iced_x86::Register::CX).unwrap() as u16,
        dx: machine.cpu.read_reg(iced_x86::Register::DX).unwrap() as u16,
        sp: machine.cpu.read_reg(iced_x86::Register::SP).unwrap() as u16,
        bp: machine.cpu.read_reg(iced_x86::Register::BP).unwrap() as u16,
        si: machine.cpu.read_reg(iced_x86::Register::SI).unwrap() as u16,
        di: machine.cpu.read_reg(iced_x86::Register::DI).unwrap() as u16,
        flags: machine.cpu.rflags() as u16,
    }
}

fn qemu_available() -> bool {
    std::process::Command::new("qemu-system-i386")
        .arg("--version")
        .output()
        .is_ok()
}

fn build_conformance_boot_sector(init: RealRegs16, test: &[u8]) -> [u8; 512] {
    const BUF: u16 = 0x0500;
    const OUT_LEN: u16 = 18;

    let mut code: Vec<u8> = Vec::new();

    // Basic real-mode setup.
    code.extend_from_slice(&[
        0xFA, // cli
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xD8, // mov ds,ax
        0x8E, 0xC0, // mov es,ax
        0x8E, 0xD0, // mov ss,ax
    ]);

    // Initialize stack first so test code can use it.
    code.extend_from_slice(&[0xBC, (init.sp & 0xFF) as u8, (init.sp >> 8) as u8]); // mov sp,imm16

    // Initialize registers.
    code.extend_from_slice(&[0xB8, (init.ax & 0xFF) as u8, (init.ax >> 8) as u8]); // mov ax,imm16
    code.extend_from_slice(&[0xBB, (init.bx & 0xFF) as u8, (init.bx >> 8) as u8]); // mov bx,imm16
    code.extend_from_slice(&[0xB9, (init.cx & 0xFF) as u8, (init.cx >> 8) as u8]); // mov cx,imm16
    code.extend_from_slice(&[0xBA, (init.dx & 0xFF) as u8, (init.dx >> 8) as u8]); // mov dx,imm16
    code.extend_from_slice(&[0xBE, (init.si & 0xFF) as u8, (init.si >> 8) as u8]); // mov si,imm16
    code.extend_from_slice(&[0xBF, (init.di & 0xFF) as u8, (init.di >> 8) as u8]); // mov di,imm16
    code.extend_from_slice(&[0xBD, (init.bp & 0xFF) as u8, (init.bp >> 8) as u8]); // mov bp,imm16

    // Execute test bytes.
    code.extend_from_slice(test);

    // Store regs to [BUF..].
    // mov [imm16], ax  (A3 iw)
    code.extend_from_slice(&[0xA3, (BUF & 0xFF) as u8, (BUF >> 8) as u8]);
    // mov [imm16], bx  (89 1E iw)
    code.extend_from_slice(&[
        0x89,
        0x1E,
        (BUF.wrapping_add(2) & 0xFF) as u8,
        (BUF.wrapping_add(2) >> 8) as u8,
    ]);
    code.extend_from_slice(&[
        0x89,
        0x0E,
        (BUF.wrapping_add(4) & 0xFF) as u8,
        (BUF.wrapping_add(4) >> 8) as u8,
    ]); // cx
    code.extend_from_slice(&[
        0x89,
        0x16,
        (BUF.wrapping_add(6) & 0xFF) as u8,
        (BUF.wrapping_add(6) >> 8) as u8,
    ]); // dx
    code.extend_from_slice(&[
        0x89,
        0x26,
        (BUF.wrapping_add(8) & 0xFF) as u8,
        (BUF.wrapping_add(8) >> 8) as u8,
    ]); // sp
    code.extend_from_slice(&[
        0x89,
        0x2E,
        (BUF.wrapping_add(10) & 0xFF) as u8,
        (BUF.wrapping_add(10) >> 8) as u8,
    ]); // bp
    code.extend_from_slice(&[
        0x89,
        0x36,
        (BUF.wrapping_add(12) & 0xFF) as u8,
        (BUF.wrapping_add(12) >> 8) as u8,
    ]); // si
    code.extend_from_slice(&[
        0x89,
        0x3E,
        (BUF.wrapping_add(14) & 0xFF) as u8,
        (BUF.wrapping_add(14) >> 8) as u8,
    ]); // di
        // flags -> ax
    code.extend_from_slice(&[0x9C, 0x58]); // pushf; pop ax
    code.extend_from_slice(&[
        0xA3,
        (BUF.wrapping_add(16) & 0xFF) as u8,
        (BUF.wrapping_add(16) >> 8) as u8,
    ]);

    // Output buffer bytes.
    code.extend_from_slice(&[0xBE, (BUF & 0xFF) as u8, (BUF >> 8) as u8]); // mov si,BUF
    code.extend_from_slice(&[0xB9, (OUT_LEN & 0xFF) as u8, (OUT_LEN >> 8) as u8]); // mov cx,OUT_LEN
    let loop_start = code.len();
    code.push(0xAC); // lodsb
    code.extend_from_slice(&[0xE6, 0xE9]); // out 0xE9,al
    code.extend_from_slice(&[0xE2, 0x00]); // loop rel8 (patched below)
    let loop_end = code.len();
    let disp = loop_start as i32 - loop_end as i32;
    code[loop_end - 1] = disp as i8 as u8;

    // Exit QEMU (isa-debug-exit).
    code.extend_from_slice(&[0xB0, 0x00, 0xE6, 0xF4]); // mov al,0; out 0xF4,al

    assert!(code.len() <= 510, "boot sector too large: {}", code.len());

    let mut img = [0u8; 512];
    img[..code.len()].copy_from_slice(&code);
    img[510] = 0x55;
    img[511] = 0xAA;
    img
}

fn run_qemu_real(test: &[u8], init: RealRegs16) -> Option<RealRegs16> {
    if !qemu_available() {
        return None;
    }

    std::fs::create_dir_all("target").ok()?;
    let img_path = "target/qemu-conformance.img";
    let out_path = "target/qemu-debugcon.bin";
    let img = build_conformance_boot_sector(init, test);
    std::fs::write(img_path, &img).ok()?;
    let _ = std::fs::remove_file(out_path);

    let _output = std::process::Command::new("qemu-system-i386")
        .args([
            "-display",
            "none",
            "-serial",
            "none",
            "-monitor",
            "none",
            "-no-reboot",
            "-drive",
            &format!("format=raw,file={img_path},if=floppy"),
            "-debugcon",
            &format!("file:{out_path}"),
            "-global",
            "isa-debugcon.iobase=0xe9",
            "-device",
            "isa-debug-exit,iobase=0xf4,iosize=0x04",
        ])
        .output()
        .ok()?;

    let bytes = std::fs::read(out_path).ok()?;
    if bytes.len() < 18 {
        return None;
    }
    let w = |off: usize| u16::from_le_bytes([bytes[off], bytes[off + 1]]);
    Some(RealRegs16 {
        ax: w(0),
        bx: w(2),
        cx: w(4),
        dx: w(6),
        sp: w(8),
        bp: w(10),
        si: w(12),
        di: w(14),
        flags: w(16),
    })
}

#[test]
#[ignore]
fn conformance_add_al_bl_against_qemu() {
    let init = RealRegs16 {
        ax: 0x00FF,
        bx: 0x0001,
        cx: 0,
        dx: 0,
        sp: 0x7C00,
        bp: 0,
        si: 0,
        di: 0,
        flags: 0x0002,
    };
    let test = [0x00, 0xD8]; // add al,bl
    let Some(qemu) = run_qemu_real(&test, init) else {
        return;
    };
    let aero = run_aero_real(&test, init);
    assert_eq!(aero, qemu);
}
