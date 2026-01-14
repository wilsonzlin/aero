#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitConfig, JitRuntime, DEFAULT_CODE_VERSION_MAX_PAGES,
};
use aero_cpu_core::state::CpuState;
use aero_jit_x86::backend::WasmtimeBackend;
use aero_jit_x86::jit_ctx;
use aero_jit_x86::wasm::JIT_EXIT_SENTINEL_I64;
use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    Instruction, MemArg, MemoryType, Module, TypeSection, ValType,
};

#[derive(Debug, Default)]
struct NullCompileSink;

impl CompileRequestSink for NullCompileSink {
    fn request_compile(&mut self, _entry_rip: u64) {
        // Tests install pre-compiled blocks directly.
    }
}

fn memarg(offset: u32, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}

fn compile_guard_block_wasm(expected: u32, ok_next_rip: u64) -> Vec<u8> {
    // A minimal WASM module exporting `block(cpu_ptr, jit_ctx_ptr) -> i64` that:
    //   - reads `CODE_VERSION_TABLE_{PTR,LEN}` from the Tier-2 context ABI slots,
    //   - loads `table[0]` when the table is enabled,
    //   - returns `JIT_EXIT_SENTINEL_I64` when the version mismatches `expected`,
    //   - otherwise returns `ok_next_rip`.
    //
    // This is intentionally a tiny hand-written module to exercise the runtime/backend coherence
    // contract: `JitRuntime::on_guest_write(..)` must update the same table that JIT code reads.
    let mut module = Module::new();

    let mut types = TypeSection::new();
    let ty_block = types.len();
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I64]);
    module.section(&types);

    let mut imports = ImportSection::new();
    // Match the reference `WasmtimeBackend` memory type: fixed 2-page wasm32 memory.
    imports.import(
        aero_jit_x86::wasm::IMPORT_MODULE,
        aero_jit_x86::wasm::IMPORT_MEMORY,
        MemoryType {
            minimum: 2,
            maximum: Some(2),
            memory64: false,
            shared: false,
            page_size_log2: None,
        },
    );
    module.section(&imports);

    let mut functions = FunctionSection::new();
    functions.function(ty_block);
    module.section(&functions);

    let mut exports = ExportSection::new();
    exports.export(aero_jit_x86::wasm::EXPORT_BLOCK_FN, ExportKind::Func, 0);
    module.section(&exports);

    let mut f = Function::new(vec![]);

    // ---- current_version = (table_len != 0) ? load_u32(table_ptr) : 0 --------------------------
    // table_len = *(cpu_ptr + LEN_OFFSET)
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Load(memarg(
        jit_ctx::CODE_VERSION_TABLE_LEN_OFFSET,
        2,
    )));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32Ne);

    // if (result i32) { ... } else { 0 }
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    {
        // table_ptr = *(cpu_ptr + PTR_OFFSET)
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Load(memarg(
            jit_ctx::CODE_VERSION_TABLE_PTR_OFFSET,
            2,
        )));
        // load_u32(table_ptr)
        f.instruction(&Instruction::I32Load(memarg(0, 2)));
    }
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::End);

    // ---- mismatch = (current_version != expected) --------------------------------------------
    f.instruction(&Instruction::I32Const(expected as i32));
    f.instruction(&Instruction::I32Ne);

    // ---- return mismatch ? sentinel : ok_next_rip ---------------------------------------------
    f.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    {
        f.instruction(&Instruction::I64Const(JIT_EXIT_SENTINEL_I64));
    }
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::I64Const(ok_next_rip as i64));
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::End);

    let mut code = CodeSection::new();
    code.function(&f);
    module.section(&code);

    module.finish()
}

#[test]
fn wasmtime_code_version_table_is_coherent_with_jit_runtime_on_guest_write() {
    // Install a block that returns `0x2000` while the table entry is 0, but returns the sentinel
    // when page 0 is bumped (expected=0, actual=1).
    let entry_rip = 0x1000u64;
    let ok_next_rip = 0x2000u64;
    let wasm = compile_guard_block_wasm(0, ok_next_rip);

    let mut backend: WasmtimeBackend<CpuState> = WasmtimeBackend::new();
    let table_index = backend.add_compiled_block(&wasm);

    let config = JitConfig {
        enabled: true,
        // Keep hotness-based compilation out of the way; this test installs a precompiled handle.
        hot_threshold: 1_000_000,
        cache_max_blocks: 16,
        cache_max_bytes: 0,
        code_version_max_pages: DEFAULT_CODE_VERSION_MAX_PAGES,
    };
    let mut jit = JitRuntime::new(config, backend, NullCompileSink);

    // Install with `byte_len=0` so the host-side snapshot validator does not prevent the guard from
    // executing (the guard itself is what this test is exercising).
    jit.install_block(entry_rip, table_index, 0, 0);

    // Entry should be valid and return `ok_next_rip` while version==expected.
    let handle = jit
        .prepare_block(entry_rip)
        .expect("block should be installed");
    let mut cpu = CpuState {
        rip: entry_rip,
        ..Default::default()
    };
    let exit = jit.execute_block(&mut cpu, &handle);
    assert!(!exit.exit_to_interpreter);
    assert_eq!(exit.next_rip, ok_next_rip);

    // Bump the page version on the host side. Coherence requirement: this must also update the
    // JIT-visible table inside Wasmtime linear memory, so the next execution sees the mismatch.
    jit.on_guest_write(0, 1);
    assert_eq!(
        jit.page_versions().version(0),
        1,
        "host bump should update the authoritative (shared) code-version table"
    );

    cpu.rip = entry_rip;
    let exit2 = jit.execute_block(&mut cpu, &handle);
    assert!(
        exit2.exit_to_interpreter,
        "guard should trip after the host bumps the page version"
    );
    assert_eq!(exit2.next_rip, entry_rip);
}
