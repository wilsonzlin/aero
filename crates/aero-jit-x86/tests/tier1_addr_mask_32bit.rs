mod tier1_common;

use std::collections::HashMap;

use aero_cpu_core::state::{CpuMode, CpuState};
use aero_jit_x86::tier1::ir::interp::execute_block;
use aero_jit_x86::{
    discover_block_mode, translate_block, BasicBlock, BlockEndKind, BlockLimits, Tier1Bus,
};
use aero_types::Gpr;
use aero_x86::tier1::decode_one_mode;
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot};

#[derive(Default)]
struct MapBus {
    mem: HashMap<u64, u8>,
}

impl MapBus {
    fn write_le(&mut self, addr: u64, value: u32) {
        let bytes = value.to_le_bytes();
        for (i, b) in bytes.into_iter().enumerate() {
            self.write_u8(addr + i as u64, b);
        }
    }
}

impl Tier1Bus for MapBus {
    fn read_u8(&self, addr: u64) -> u8 {
        *self.mem.get(&addr).unwrap_or(&0)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.mem.insert(addr, value);
    }
}

#[test]
fn tier1_masks_32bit_effective_addresses_for_memory_operands() {
    // mov eax, [edi]
    let entry_rip = 0x1000u64;
    let bytes = [0x8b, 0x07];
    let inst = decode_one_mode(entry_rip, &bytes, 32);
    let block = BasicBlock {
        entry_rip,
        bitness: 32,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    // Seed guest memory at address 0 with a known value.
    let mut bus = MapBus::default();
    bus.write_le(0, 0x1122_3344);

    // Set EDI with high bits set; 32-bit addressing should ignore them.
    let mut cpu = CpuState::new(CpuMode::Protected);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rdi.as_u8() as usize] = 0x1_0000_0000;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    let out = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out.gpr[Gpr::Rax.as_u8() as usize], 0x1122_3344);
}

#[test]
fn tier1_masks_32bit_stack_pointer_for_pop() {
    // pop eax
    let entry_rip = 0x2000u64;
    let bytes = [0x58];
    let inst = decode_one_mode(entry_rip, &bytes, 32);
    let block = BasicBlock {
        entry_rip,
        bitness: 32,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    // Place a 32-bit value at address 0; if ESP is masked, POP reads it.
    let mut bus = MapBus::default();
    bus.write_le(0, 0x5566_7788);

    // Set RSP with high bits set; 32-bit stack semantics should use the low 32 bits.
    let mut cpu = CpuState::new(CpuMode::Protected);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0000_0000;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    let out = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out.gpr[Gpr::Rax.as_u8() as usize], 0x5566_7788);
    assert_eq!(out.gpr[Gpr::Rsp.as_u8() as usize], 4);
}

#[test]
fn discover_block_mode_32bit_wraps_rip_between_instructions() {
    // This exercises IP wraparound without requiring an instruction to cross the 4GiB boundary.
    //
    //   0xFFFF_FFFE: inc eax  (0x40)
    //   0xFFFF_FFFF: dec ecx  (0x49)
    //   0x0000_0000: ret      (0xC3)
    let mut bus = MapBus::default();
    bus.write_u8(0xffff_fffe, 0x40);
    bus.write_u8(0xffff_ffff, 0x49);
    bus.write_u8(0x0000_0000, 0xc3);

    let block = discover_block_mode(&bus, 0xffff_fffe, BlockLimits::default(), 32);
    assert_eq!(block.entry_rip, 0xffff_fffe);
    assert_eq!(block.insts.len(), 3);
    assert_eq!(block.insts[0].rip, 0xffff_fffe);
    assert_eq!(block.insts[1].rip, 0xffff_ffff);
    assert_eq!(block.insts[2].rip, 0x0000_0000);
    assert!(matches!(block.end_kind, BlockEndKind::Ret));
}
