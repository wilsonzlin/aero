//! Shared instruction emission helpers for the inline JIT TLB fast-path.
//!
//! Tier-1 and Tier-2 WASM codegens both emit identical inline-TLB probes and related helpers
//! (direct-mapped TLB tag check, RAM address computation, and code-version bumping). Keep the
//! sequences here to avoid semantic drift between tiers.

use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

use crate::jit_ctx::JitContext;

#[derive(Debug, Clone, Copy)]
pub(crate) struct InlineTlbLocals {
    pub(crate) cpu_ptr: u32,
    pub(crate) jit_ctx_ptr: u32,
    pub(crate) ram_base: u32,
    pub(crate) tlb_salt: u32,
    pub(crate) scratch_vaddr: u32,
    pub(crate) scratch_vpn: u32,
    pub(crate) scratch_tlb_data: u32,
    pub(crate) code_version_table_ptr: Option<u32>,
    pub(crate) code_version_table_len: Option<u32>,
}

pub(crate) fn emit_translate_and_cache(
    f: &mut Function,
    depth: &mut u32,
    locals: InlineTlbLocals,
    mmu_translate_fn: u32,
    access_code: i32,
    required_flag: u64,
) {
    // vpn = vaddr >> 12
    f.instruction(&Instruction::LocalGet(locals.scratch_vaddr));
    f.instruction(&Instruction::I64Const(crate::PAGE_SHIFT as i64));
    f.instruction(&Instruction::I64ShrU);
    f.instruction(&Instruction::LocalSet(locals.scratch_vpn));

    // Check TLB tag match.
    emit_tlb_entry_addr(f, locals);
    f.instruction(&Instruction::I64Load(memarg(0, 3))); // tag
    f.instruction(&Instruction::LocalGet(locals.scratch_vpn));
    f.instruction(&Instruction::LocalGet(locals.tlb_salt));
    f.instruction(&Instruction::I64Xor);
    // expect_tag = (vpn ^ salt) | 1; keep 0 reserved for invalidation.
    f.instruction(&Instruction::I64Const(1));
    f.instruction(&Instruction::I64Or);
    f.instruction(&Instruction::I64Eq);

    f.instruction(&Instruction::If(BlockType::Empty));
    *depth += 1;
    {
        // Hit: load `data` from the entry.
        emit_tlb_entry_addr(f, locals);
        f.instruction(&Instruction::I64Load(memarg(8, 3))); // data
        f.instruction(&Instruction::LocalSet(locals.scratch_tlb_data));
    }
    f.instruction(&Instruction::Else);
    {
        // Miss: call the translation helper (expected to fill the entry).
        emit_mmu_translate(f, locals, mmu_translate_fn, access_code);
    }
    f.instruction(&Instruction::End);
    *depth -= 1;

    // Permission check: if the cached entry doesn't permit this access, go slow-path.
    f.instruction(&Instruction::LocalGet(locals.scratch_tlb_data));
    f.instruction(&Instruction::I64Const(required_flag as i64));
    f.instruction(&Instruction::I64And);
    f.instruction(&Instruction::I64Eqz);

    f.instruction(&Instruction::If(BlockType::Empty));
    *depth += 1;
    {
        emit_mmu_translate(f, locals, mmu_translate_fn, access_code);
    }
    f.instruction(&Instruction::End);
    *depth -= 1;
}

pub(crate) fn emit_compute_ram_addr(f: &mut Function, locals: InlineTlbLocals) {
    // paddr = (phys_base & !0xFFF) | (vaddr & 0xFFF)
    f.instruction(&Instruction::LocalGet(locals.scratch_tlb_data));
    f.instruction(&Instruction::I64Const(crate::PAGE_BASE_MASK as i64));
    f.instruction(&Instruction::I64And);

    f.instruction(&Instruction::LocalGet(locals.scratch_vaddr));
    f.instruction(&Instruction::I64Const(crate::PAGE_OFFSET_MASK as i64));
    f.instruction(&Instruction::I64And);
    f.instruction(&Instruction::I64Or);

    // Q35 high-memory remap: translate physical addresses in the high-RAM region back into the
    // contiguous RAM backing store used by the wasm runtime.
    //
    // If paddr >= 4GiB:
    //   paddr = LOW_RAM_END + (paddr - 4GiB)
    const HIGH_RAM_BASE: i64 = 0x1_0000_0000;
    const LOW_RAM_END: i64 = aero_pc_constants::PCIE_ECAM_BASE as i64;
    f.instruction(&Instruction::LocalTee(locals.scratch_vpn));
    f.instruction(&Instruction::I64Const(HIGH_RAM_BASE));
    f.instruction(&Instruction::I64GeU);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    {
        f.instruction(&Instruction::LocalGet(locals.scratch_vpn));
        f.instruction(&Instruction::I64Const(HIGH_RAM_BASE));
        f.instruction(&Instruction::I64Sub);
        f.instruction(&Instruction::I64Const(LOW_RAM_END));
        f.instruction(&Instruction::I64Add);
    }
    f.instruction(&Instruction::Else);
    {
        f.instruction(&Instruction::LocalGet(locals.scratch_vpn));
    }
    f.instruction(&Instruction::End);

    // wasm_addr = ram_base + paddr
    f.instruction(&Instruction::LocalGet(locals.ram_base));
    f.instruction(&Instruction::I64Add);
    f.instruction(&Instruction::I32WrapI64);
}

pub(crate) fn emit_bump_code_version_fastpath(
    f: &mut Function,
    locals: InlineTlbLocals,
    memory_shared: bool,
) {
    let code_version_table_ptr = locals
        .code_version_table_ptr
        .expect("code version table locals disabled");
    let code_version_table_len = locals
        .code_version_table_len
        .expect("code version table locals disabled");

    // If the runtime hasn't configured a version table, skip.
    f.instruction(&Instruction::LocalGet(code_version_table_len));
    f.instruction(&Instruction::I64Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::Else);
    {
        // Compute the physical page number for this store.
        f.instruction(&Instruction::LocalGet(locals.scratch_tlb_data));
        f.instruction(&Instruction::I64Const(crate::PAGE_BASE_MASK as i64));
        f.instruction(&Instruction::I64And);
        f.instruction(&Instruction::I64Const(crate::PAGE_SHIFT as i64));
        f.instruction(&Instruction::I64ShrU); // -> page (i64)
        f.instruction(&Instruction::LocalTee(locals.scratch_vpn));

        // Bounds check: page < table_len.
        f.instruction(&Instruction::LocalGet(code_version_table_len));
        f.instruction(&Instruction::I64LtU);

        f.instruction(&Instruction::If(BlockType::Empty));
        {
            // addr = table_ptr + page * 4
            f.instruction(&Instruction::LocalGet(code_version_table_ptr));
            f.instruction(&Instruction::LocalGet(locals.scratch_vpn));

            f.instruction(&Instruction::I64Const(4));
            f.instruction(&Instruction::I64Mul);
            f.instruction(&Instruction::I64Add);
            f.instruction(&Instruction::LocalSet(locals.scratch_vpn));

            // table[page] += 1
            //
            // This is wrapping arithmetic: both `i32.add` and `i32.atomic.rmw.add` wrap on
            // overflow (`0xffff_ffff + 1 == 0`), matching the runtime and JS `Atomics.add`
            // semantics.
            //
            // Note: when the module imports a shared memory, we must use atomic operations for
            // correctness under concurrency. Non-atomic `i32.load`/`i32.store` sequences can lose
            // increments when multiple agents bump the same entry concurrently.
            f.instruction(&Instruction::LocalGet(locals.scratch_vpn));
            f.instruction(&Instruction::I32WrapI64);
            if memory_shared {
                f.instruction(&Instruction::I32Const(1));
                f.instruction(&Instruction::I32AtomicRmwAdd(memarg(0, 2)));
                // `i32.atomic.rmw.add` returns the old value; we only care that the increment
                // happens.
                f.instruction(&Instruction::Drop);
            } else {
                f.instruction(&Instruction::I32Load(memarg(0, 2)));
                f.instruction(&Instruction::I32Const(1));
                f.instruction(&Instruction::I32Add);
                f.instruction(&Instruction::I64ExtendI32U);
                f.instruction(&Instruction::LocalSet(locals.scratch_vaddr));

                f.instruction(&Instruction::LocalGet(locals.scratch_vpn));
                f.instruction(&Instruction::I32WrapI64);
                f.instruction(&Instruction::LocalGet(locals.scratch_vaddr));
                f.instruction(&Instruction::I32WrapI64);
                f.instruction(&Instruction::I32Store(memarg(0, 2)));
            }
        }
        f.instruction(&Instruction::End);
    }
    f.instruction(&Instruction::End);
}

fn emit_tlb_entry_addr(f: &mut Function, locals: InlineTlbLocals) {
    // base = jit_ctx_ptr + JitContext::TLB_OFFSET + ((vpn & mask) * ENTRY_SIZE)
    f.instruction(&Instruction::LocalGet(locals.jit_ctx_ptr));
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64Const(JitContext::TLB_OFFSET as i64));
    f.instruction(&Instruction::I64Add);

    f.instruction(&Instruction::LocalGet(locals.scratch_vpn));
    f.instruction(&Instruction::I64Const(crate::JIT_TLB_INDEX_MASK as i64));
    f.instruction(&Instruction::I64And);
    f.instruction(&Instruction::I64Const(crate::JIT_TLB_ENTRY_SIZE as i64));
    f.instruction(&Instruction::I64Mul);
    f.instruction(&Instruction::I64Add);
    f.instruction(&Instruction::I32WrapI64);
}

fn emit_mmu_translate(
    f: &mut Function,
    locals: InlineTlbLocals,
    mmu_translate_fn: u32,
    access_code: i32,
) {
    f.instruction(&Instruction::LocalGet(locals.cpu_ptr));
    f.instruction(&Instruction::LocalGet(locals.jit_ctx_ptr));
    f.instruction(&Instruction::LocalGet(locals.scratch_vaddr));
    f.instruction(&Instruction::I32Const(access_code));
    f.instruction(&Instruction::Call(mmu_translate_fn));
    f.instruction(&Instruction::LocalSet(locals.scratch_tlb_data));
}

fn memarg(offset: u32, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}
