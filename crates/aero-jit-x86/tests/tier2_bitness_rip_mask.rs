mod tier1_common;

use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use tier1_common::SimpleBus;

#[test]
fn tier2_cfg_builder_masks_32bit_entry_rip_for_block_keys() {
    // jmp -2 (to self)
    let code = [
        0xeb, 0xfe, // jmp 0x0000_0000
    ];

    let mut bus = SimpleBus::new(0x1000);
    bus.load(0, &code);

    // 32-bit guests architecturally mask EIP to 32 bits; an entry RIP with high bits set should
    // still build a CFG rooted at address 0 without creating duplicate blocks.
    let func = build_function_from_x86(&bus, 0x1_0000_0000, 32, CfgBuildConfig::default());

    assert_eq!(func.block(func.entry).start_rip, 0);
    assert_eq!(func.find_block_by_rip(0), Some(func.entry));

    assert_eq!(
        func.blocks.iter().filter(|b| b.start_rip == 0).count(),
        1,
        "unexpected duplicate blocks for masked 32-bit RIP"
    );
}
