#![cfg(all(debug_assertions, not(target_arch = "wasm32")))]

mod tier1_common;

use aero_cpu_core::state::{CpuMode, CpuState};
use aero_jit_x86::tier1::ir::interp::execute_block;
use aero_jit_x86::{translate_block, BasicBlock, BlockEndKind};
use aero_types::Gpr;
use aero_x86::tier1::decode_one_mode;
use tier1_common::{write_cpu_to_wasm_bytes, CpuSnapshot, SimpleBus};

#[test]
fn tier1_masks_16bit_effective_addresses_for_memory_operands() {
    // mov ax, [di]
    let entry_rip = 0x1000u64;
    let bytes = [0x8b, 0x05];
    let inst = decode_one_mode(entry_rip, &bytes, 16);
    let block = BasicBlock {
        entry_rip,
        bitness: 16,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    let mut bus = SimpleBus::new(0x4000);
    bus.load(0, &0x1122u16.to_le_bytes());

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rdi.as_u8() as usize] = 0x1_0000;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    let out = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out.gpr[Gpr::Rax.as_u8() as usize], 0x1122);
}

#[test]
fn tier1_masks_16bit_effective_addresses_for_memory_stores() {
    // mov [di], ax
    let entry_rip = 0x1800u64;
    let bytes = [0x89, 0x05];
    let inst = decode_one_mode(entry_rip, &bytes, 16);
    let block = BasicBlock {
        entry_rip,
        bitness: 16,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    let mut bus = SimpleBus::new(0x4000);

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rdi.as_u8() as usize] = 0x1_0000;
    cpu.gpr[Gpr::Rax.as_u8() as usize] = 0xdead_beefu64;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    assert_eq!(&bus.mem()[0..2], &0xbeef_u16.to_le_bytes());
}

#[test]
fn tier1_masks_16bit_stack_pointer_for_pop() {
    // pop ax
    let entry_rip = 0x2000u64;
    let bytes = [0x58];
    let inst = decode_one_mode(entry_rip, &bytes, 16);
    let block = BasicBlock {
        entry_rip,
        bitness: 16,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    let mut bus = SimpleBus::new(0x4000);
    bus.load(0, &0x3344u16.to_le_bytes());

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0000;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    let out = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out.gpr[Gpr::Rax.as_u8() as usize], 0x3344);
    assert_eq!(out.gpr[Gpr::Rsp.as_u8() as usize], 2);
}

#[test]
fn tier1_masks_16bit_stack_pointer_for_push() {
    // push ax
    let entry_rip = 0x2800u64;
    let bytes = [0x50];
    let inst = decode_one_mode(entry_rip, &bytes, 16);
    let block = BasicBlock {
        entry_rip,
        bitness: 16,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    let mut bus = SimpleBus::new(0x4000);

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1122_3344u64;
    cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0002;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    let out = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out.gpr[Gpr::Rsp.as_u8() as usize], 0);
    assert_eq!(&bus.mem()[0..2], &0x3344u16.to_le_bytes());
}
#[test]
fn tier1_masks_16bit_stack_pointer_wraps_on_push() {
    // push ax
    let entry_rip = 0x3000u64;
    let bytes = [0x50];
    let inst = decode_one_mode(entry_rip, &bytes, 16);
    let block = BasicBlock {
        entry_rip,
        bitness: 16,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    let mut bus = SimpleBus::new(0x10000);

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1122_3344u64;
    cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0000;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    let out = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out.gpr[Gpr::Rsp.as_u8() as usize], 0xfffe);
    assert_eq!(&bus.mem()[0xfffe..0x10000], &0x3344u16.to_le_bytes());
}

#[test]
fn tier1_masks_16bit_stack_pointer_wraps_on_pop() {
    // pop ax
    let entry_rip = 0x3800u64;
    let bytes = [0x58];
    let inst = decode_one_mode(entry_rip, &bytes, 16);
    let block = BasicBlock {
        entry_rip,
        bitness: 16,
        insts: vec![inst],
        end_kind: BlockEndKind::Limit {
            next_rip: entry_rip + bytes.len() as u64,
        },
    };
    let ir = translate_block(&block);

    let mut bus = SimpleBus::new(0x10000);
    bus.load(0xfffe, &0x5566u16.to_le_bytes());

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.rip = entry_rip;
    cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_fffe;

    let mut cpu_bytes = vec![0u8; aero_jit_x86::abi::CPU_STATE_SIZE as usize];
    write_cpu_to_wasm_bytes(&cpu, &mut cpu_bytes);
    let _ = execute_block(&ir, &mut cpu_bytes, &mut bus);

    let out = CpuSnapshot::from_wasm_bytes(&cpu_bytes);
    assert_eq!(out.gpr[Gpr::Rax.as_u8() as usize], 0x5566);
    assert_eq!(out.gpr[Gpr::Rsp.as_u8() as usize], 0);
}
