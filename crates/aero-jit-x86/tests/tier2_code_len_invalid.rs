mod tier1_common;

use aero_cpu_core::jit::runtime::PageVersionTracker;
use aero_jit_x86::tier2::ir::Instr;
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use tier1_common::SimpleBus;

#[test]
fn tier2_code_version_guard_ignores_trailing_invalid_page() {
    // Place an executed instruction at the last byte of a page, followed by an unsupported opcode
    // on the next page:
    //   0x0FFF: push rbx  (1 byte, executed)
    //   0x1000: cmc       (decoded as Invalid; Tier-1/Tier-2 side-exit at 0x1000; not executed)
    //
    // Tier-2 `Block::code_len` is expected to cover only executed bytes, so trace code-version
    // guards should not include the second page.
    let entry = 0x0fff_u64;
    let mut bus = SimpleBus::new(0x3000);
    bus.load(entry, &[0x53]); // push rbx
    bus.load(0x1000, &[0xf5]); // cmc (unsupported by Tier-1 => Invalid terminator)

    let func = build_function_from_x86(&bus, entry, 64, CfgBuildConfig::default());
    assert_eq!(func.block(func.entry).code_len, 1);

    let mut profile = ProfileData::default();
    profile.block_counts.insert(func.entry, 10_000);

    let page_versions = PageVersionTracker::default();
    let builder = TraceBuilder::new(
        &func,
        &profile,
        &page_versions,
        TraceConfig {
            hot_block_threshold: 1000,
            max_blocks: 8,
            max_instrs: 256,
        },
    );
    let trace = builder.build_from(func.entry).expect("trace should be hot");

    let entry_page = entry >> aero_jit_x86::PAGE_SHIFT;
    let guarded_pages: Vec<u64> = trace
        .ir
        .prologue
        .iter()
        .filter_map(|inst| match inst {
            Instr::GuardCodeVersion { page, .. } => Some(*page),
            _ => None,
        })
        .collect();
    assert_eq!(guarded_pages, vec![entry_page]);
}
