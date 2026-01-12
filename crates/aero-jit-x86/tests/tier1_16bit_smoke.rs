mod tier1_common;

use aero_jit_x86::compiler::tier1::compile_tier1_block;
use aero_jit_x86::BlockLimits;
use tier1_common::SimpleBus;

#[test]
fn tier1_compiles_16bit_block_smoke() {
    // NOTE: The browser Tier-1 smoke harness executes guest code in 16-bit real mode.
    // This regression test ensures the Tier-1 translator does not panic on `bitness=16`.
    //
    // Guest bytes:
    //   66 83 C0 01    add eax, 1   (operand-size override in 16-bit mode)
    //   EB FA          jmp short -6
    let entry_rip = 0x1000u64;
    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry_rip, &[0x66, 0x83, 0xc0, 0x01, 0xeb, 0xfa]);

    let compilation = compile_tier1_block(&bus, entry_rip, 16, BlockLimits::default())
        .expect("Tier-1 compilation should succeed for the 16-bit hot-loop block");

    assert_eq!(compilation.entry_rip, entry_rip);
    assert_eq!(compilation.byte_len, 6);
    assert!(!compilation.exit_to_interpreter);
    assert!(
        compilation
            .wasm_bytes
            .starts_with(&[0x00, 0x61, 0x73, 0x6d]),
        "expected wasm binary header"
    );
}
