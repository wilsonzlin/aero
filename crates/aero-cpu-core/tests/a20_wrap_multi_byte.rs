use std::collections::BTreeMap;

use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::Exception;
use aero_x86::Register;

fn run_to_halt<B: CpuBus>(state: &mut CpuState, bus: &mut B, max: u64) {
    let mut steps = 0u64;
    while steps < max {
        let res = run_batch(state, bus, 1024);
        steps += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => return,
            BatchExit::BiosInterrupt(vector) => panic!("unexpected BIOS interrupt: {vector:#x}"),
            BatchExit::Assist(r) => panic!("unexpected assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception: {e:?}"),
        }
    }
    panic!("program did not halt");
}

#[test]
fn real_mode_a20_disabled_mov_ax_load_wraps_across_1mib() {
    // mov ax, [0x000FFFFF] (moffs32); hlt
    let code = [
        0x67, 0xA1, 0xFF, 0xFF, 0x0F, 0x00, // addr-size override + MOV AX, moffs32
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x100000);
    bus.load(0x200, &code);

    // Place distinct bytes at 0xFFFFF and 0x00000 so the u16 crosses the A20 alias boundary.
    bus.write_u8(0xFFFFF, 0x34).unwrap();
    bus.write_u8(0x00000, 0x12).unwrap();

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(0x200);
    state.a20_enabled = false;

    run_to_halt(&mut state, &mut bus, 16);
    assert_eq!(state.read_reg(Register::AX), 0x1234);
}

#[test]
fn real_mode_a20_disabled_mov_ax_store_wraps_across_1mib() {
    // mov ax, 0x1234; mov [0x000FFFFF], ax (moffs32); hlt
    let code = [
        0xB8, 0x34, 0x12, // mov ax, 0x1234
        0x67, 0xA3, 0xFF, 0xFF, 0x0F, 0x00, // addr-size override + MOV moffs32, AX
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x100000);
    bus.load(0x200, &code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(0x200);
    state.a20_enabled = false;

    run_to_halt(&mut state, &mut bus, 16);

    assert_eq!(bus.read_u8(0xFFFFF).unwrap(), 0x34);
    assert_eq!(bus.read_u8(0x00000).unwrap(), 0x12);
}

#[derive(Default)]
struct SparseBus {
    mem: BTreeMap<u64, u8>,
}

impl SparseBus {
    fn reserve_range(&mut self, start: u64, len: usize) {
        for i in 0..len {
            self.mem.entry(start + i as u64).or_insert(0);
        }
    }

    fn load(&mut self, addr: u64, data: &[u8]) {
        for (i, b) in data.iter().copied().enumerate() {
            self.mem.insert(addr + i as u64, b);
        }
    }

    fn byte(&self, addr: u64) -> u8 {
        *self.mem.get(&addr).expect("address should exist")
    }
}

impl CpuBus for SparseBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.mem.get(&vaddr).copied().ok_or(Exception::MemoryFault)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let mut buf = [0u8; 2];
        for i in 0..2 {
            let addr = vaddr.checked_add(i).ok_or(Exception::MemoryFault)?;
            buf[i as usize] = self.read_u8(addr)?;
        }
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut buf = [0u8; 4];
        for i in 0..4 {
            let addr = vaddr.checked_add(i).ok_or(Exception::MemoryFault)?;
            buf[i as usize] = self.read_u8(addr)?;
        }
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut buf = [0u8; 8];
        for i in 0..8 {
            let addr = vaddr.checked_add(i).ok_or(Exception::MemoryFault)?;
            buf[i as usize] = self.read_u8(addr)?;
        }
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut buf = [0u8; 16];
        for i in 0..16 {
            let addr = vaddr.checked_add(i).ok_or(Exception::MemoryFault)?;
            buf[i as usize] = self.read_u8(addr)?;
        }
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        match self.mem.get_mut(&vaddr) {
            Some(slot) => {
                *slot = val;
                Ok(())
            }
            None => Err(Exception::MemoryFault),
        }
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        for (i, b) in val.to_le_bytes().into_iter().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            self.write_u8(addr, b)?;
        }
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        for (i, b) in val.to_le_bytes().into_iter().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            self.write_u8(addr, b)?;
        }
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        for (i, b) in val.to_le_bytes().into_iter().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            self.write_u8(addr, b)?;
        }
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        for (i, b) in val.to_le_bytes().into_iter().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            self.write_u8(addr, b)?;
        }
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            buf[i] = self.read_u8(addr)?;
        }
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}

#[test]
fn protected_mode_32bit_linear_wrap_loads_across_4gib() {
    // mov eax, [0xFFFF_FFFF]; hlt
    let code = [0xA1, 0xFF, 0xFF, 0xFF, 0xFF, 0xF4];
    let code_addr = 0x1000u64;

    let mut bus = SparseBus::default();
    bus.reserve_range(code_addr, 32);
    bus.load(code_addr, &code);

    // Bytes for the wrapped dword load: addr=0xFFFF_FFFF, then 0,1,2.
    bus.reserve_range(0, 8);
    bus.reserve_range(0xFFFF_FFFF, 1);
    bus.write_u8(0xFFFF_FFFF, 0x78).unwrap();
    bus.write_u8(0, 0x56).unwrap();
    bus.write_u8(1, 0x34).unwrap();
    bus.write_u8(2, 0x12).unwrap();

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(code_addr);

    run_to_halt(&mut state, &mut bus, 16);
    assert_eq!(state.read_reg(Register::EAX), 0x1234_5678);
}

#[test]
fn protected_mode_32bit_linear_wrap_stores_across_4gib() {
    // mov eax, 0x11223344; mov [0xFFFF_FFFF], eax; hlt
    let code = [
        0xB8, 0x44, 0x33, 0x22, 0x11, // mov eax, 0x11223344
        0xA3, 0xFF, 0xFF, 0xFF, 0xFF, // mov moffs32, eax
        0xF4, // hlt
    ];
    let code_addr = 0x1000u64;

    let mut bus = SparseBus::default();
    bus.reserve_range(code_addr, 32);
    bus.load(code_addr, &code);

    // Reserve destination bytes: 0xFFFF_FFFF, then 0,1,2.
    bus.reserve_range(0, 8);
    bus.reserve_range(0xFFFF_FFFF, 1);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(code_addr);

    run_to_halt(&mut state, &mut bus, 16);

    assert_eq!(bus.byte(0xFFFF_FFFF), 0x44);
    assert_eq!(bus.byte(0), 0x33);
    assert_eq!(bus.byte(1), 0x22);
    assert_eq!(bus.byte(2), 0x11);
}

#[test]
fn tier0_fetch_wraps_across_a20_alias_boundary() {
    // Place `mov al, 0x5A` such that the opcode is at 0xFFFFF and the immediate is at 0x00000.
    // After executing, IP wraps (16-bit) and execution continues at CS:0x0001.
    let mut bus = FlatTestBus::new(0x100000);
    bus.write_u8(0xFFFFF, 0xB0).unwrap(); // mov al, imm8
    bus.write_u8(0x00000, 0x5A).unwrap(); // imm8
    bus.write_u8(0xF0001, 0xF4).unwrap(); // hlt at CS:0x0001 (CS=0xF000)

    let mut state = CpuState::new(CpuMode::Bit16);
    state.write_reg(Register::CS, 0xF000);
    state.set_rip(0xFFFF);
    state.a20_enabled = false;

    run_to_halt(&mut state, &mut bus, 16);
    assert_eq!(state.read_reg(Register::AL), 0x5A);
}

#[test]
fn tier0_fetch_wraps_across_32bit_linear_boundary() {
    // Place `mov al, 0xA5` such that the opcode is at 0xFFFF_FFFF and the immediate is at 0x0.
    let mut bus = SparseBus::default();
    bus.reserve_range(0, 32);
    bus.reserve_range(0xFFFF_FFFF, 1);
    bus.write_u8(0xFFFF_FFFF, 0xB0).unwrap(); // mov al, imm8
    bus.write_u8(0x0000_0000, 0xA5).unwrap(); // imm8
    bus.write_u8(0x0000_0001, 0xF4).unwrap(); // hlt

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0xFFFF_FFFF);

    run_to_halt(&mut state, &mut bus, 16);
    assert_eq!(state.read_reg(Register::AL), 0xA5);
}

