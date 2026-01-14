#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::jit::runtime::JitBackend;
use aero_cpu_core::state::CpuState;
use aero_jit_x86::backend::WasmtimeBackend;
use aero_jit_x86::jit_ctx;
use aero_jit_x86::tier1::ir::{IrBuilder, IrTerminator};
use aero_jit_x86::{Tier1Bus, Tier1WasmCodegen};
use aero_types::Width;

fn read_u32_le(bus: &impl Tier1Bus, addr: u64) -> u32 {
    let mut buf = [0u8; 4];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = bus.read_u8(addr + i as u64);
    }
    u32::from_le_bytes(buf)
}

#[test]
fn wasmtime_backend_initializes_code_version_table_and_bumps_on_mem_write() {
    let memory_pages = 2u32;
    let cpu_ptr = WasmtimeBackend::<CpuState>::DEFAULT_CPU_PTR;

    let mut backend = WasmtimeBackend::<CpuState>::new_with_memory_pages(memory_pages, cpu_ptr);

    let ptr_off = cpu_ptr as u64 + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as u64;
    let len_off = cpu_ptr as u64 + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as u64;
    let table_ptr = read_u32_le(&backend, ptr_off);
    let table_len = read_u32_le(&backend, len_off);

    assert!(table_len > 0, "expected a non-empty code-version table");

    let expected_ptr = (cpu_ptr as u32) + jit_ctx::TIER2_CTX_OFFSET + jit_ctx::JIT_CTX_SIZE;
    assert_eq!(
        table_ptr, expected_ptr,
        "table should follow the Tier-2 context"
    );

    let expected_len = {
        let cpu_ptr_u64 = u64::try_from(cpu_ptr).expect("cpu_ptr must be non-negative");
        u32::try_from(
            cpu_ptr_u64
                .saturating_add(aero_jit_x86::PAGE_SIZE - 1)
                / aero_jit_x86::PAGE_SIZE,
        )
        .unwrap()
    };
    assert_eq!(
        table_len, expected_len,
        "table should cover guest RAM pages [0..cpu_ptr)"
    );

    let memory_bytes = u64::from(memory_pages) * 65_536;
    let table_end = u64::from(table_ptr) + u64::from(table_len) * 4;
    assert!(
        table_end <= memory_bytes,
        "code-version table must fit in linear memory"
    );

    // Table should start zeroed.
    assert_eq!(read_u32_le(&backend, u64::from(table_ptr)), 0);

    // Execute a tiny Tier-1 block that stores one byte to guest RAM address 0. The imported
    // `mem_write_u8` helper should bump the code-version entry for page 0.
    let entry_rip = 0x1000u64;
    let mut b = IrBuilder::new(entry_rip);
    let addr = b.const_int(Width::W64, 0);
    let value = b.const_int(Width::W8, 0xAA);
    b.store(Width::W8, addr, value);
    let ir = b.finish(IrTerminator::ExitToInterpreter { next_rip: entry_rip });
    let wasm = Tier1WasmCodegen::new().compile_block(&ir);

    let table_index = backend.add_compiled_block(&wasm);
    let mut cpu = CpuState {
        rip: entry_rip,
        ..Default::default()
    };
    let _exit = backend.execute(table_index, &mut cpu);

    assert_eq!(
        read_u32_le(&backend, u64::from(table_ptr)),
        1,
        "mem_write_u8 should bump the code-version entry for the written page"
    );
}

#[test]
fn wasmtime_backend_bumps_both_pages_for_cross_page_mem_write() {
    let memory_pages = 2u32;
    let cpu_ptr = WasmtimeBackend::<CpuState>::DEFAULT_CPU_PTR;

    let mut backend = WasmtimeBackend::<CpuState>::new_with_memory_pages(memory_pages, cpu_ptr);

    let ptr_off = cpu_ptr as u64 + jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET as u64;
    let len_off = cpu_ptr as u64 + jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET as u64;
    let table_ptr = u64::from(read_u32_le(&backend, ptr_off));
    let table_len = read_u32_le(&backend, len_off);
    assert!(
        table_len > 1,
        "expected code-version table to cover at least pages 0 and 1 (len={table_len})"
    );

    // Cross-page store: write an 8-byte value at the last byte of page 0 (0xFFF), which spans into
    // page 1. The imported `mem_write_u64` helper should bump both page 0 and page 1 entries.
    let entry_rip = 0x2000u64;
    let mut b = IrBuilder::new(entry_rip);
    let addr = b.const_int(Width::W64, aero_jit_x86::PAGE_SIZE - 1);
    let value = b.const_int(Width::W64, 0x1122_3344_5566_7788);
    b.store(Width::W64, addr, value);
    let ir = b.finish(IrTerminator::ExitToInterpreter { next_rip: entry_rip });
    let wasm = Tier1WasmCodegen::new().compile_block(&ir);

    let table_index = backend.add_compiled_block(&wasm);
    let mut cpu = CpuState {
        rip: entry_rip,
        ..Default::default()
    };
    let _exit = backend.execute(table_index, &mut cpu);

    let page0_off = table_ptr;
    let page1_off = table_ptr + 4;
    assert_eq!(
        read_u32_le(&backend, page0_off),
        1,
        "mem_write_u64 should bump page 0 for a cross-page store"
    );
    assert_eq!(
        read_u32_le(&backend, page1_off),
        1,
        "mem_write_u64 should bump page 1 for a cross-page store"
    );

    // Sanity-check the store wrote through guest RAM.
    let got = (0..8u64)
        .map(|i| backend.read_u8((aero_jit_x86::PAGE_SIZE - 1) + i))
        .collect::<Vec<_>>();
    assert_eq!(got, 0x1122_3344_5566_7788u64.to_le_bytes());
}
