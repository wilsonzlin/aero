use aero_jit_x86::compiler::tier1::compile_tier1_block_with_options;
use aero_jit_x86::tier1::{BlockLimits, Tier1WasmOptions};
use aero_jit_x86::Tier1Bus;

/// Simple immutable bus over a byte slice starting at `entry_rip`.
struct SliceBus {
    entry_rip: u64,
    code: Vec<u8>,
}

impl Tier1Bus for SliceBus {
    fn read_u8(&self, addr: u64) -> u8 {
        let Some(off) = addr.checked_sub(self.entry_rip) else {
            return 0;
        };
        let Ok(off) = usize::try_from(off) else {
            return 0;
        };
        *self.code.get(off).unwrap_or(&0)
    }

    fn write_u8(&mut self, _addr: u64, _value: u8) {
        // Tier-1 compilation should never mutate via the bus.
    }
}

#[test]
fn tier1_can_compile_16bit_decode_mode_blocks() {
    let entry_rip = 0x1000u64;
    // Real-mode-ish hot loop:
    //   add eax, 1   (operand-size override in 16-bit mode)
    //   jmp short -6
    //
    // This is used by the tiered runtime smoke harness.
    let code = vec![0x66, 0x83, 0xc0, 0x01, 0xeb, 0xfa];
    let bus = SliceBus {
        entry_rip,
        code,
    };

    let limits = BlockLimits {
        max_insts: 64,
        max_bytes: 1024,
    };
    let options = Tier1WasmOptions::default();

    let compilation = compile_tier1_block_with_options(&bus, entry_rip, 16, limits, options)
        .expect("Tier-1 compilation succeeds for bitness=16");

    assert_eq!(compilation.entry_rip, entry_rip);
    assert_eq!(compilation.byte_len, 6);
    assert!(!compilation.wasm_bytes.is_empty());

    // Ensure the emitted block is a valid standalone wasm module (equivalent to `WebAssembly.validate`).
    let mut validator = wasmparser::Validator::new();
    validator
        .validate_all(&compilation.wasm_bytes)
        .expect("compiled Tier-1 block must be valid wasm");
}

