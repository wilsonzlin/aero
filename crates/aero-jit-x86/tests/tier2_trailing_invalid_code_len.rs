mod tier1_common;

use aero_jit_x86::tier2::interp::RuntimeEnv;
use aero_jit_x86::tier2::ir::Instr;
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use tier1_common::SimpleBus;

#[test]
fn tier2_trace_builder_ignores_trailing_invalid_page() {
    // Place an executed instruction at the last byte of a page, followed by an unsupported opcode
    // on the next page:
    //   0x0FFF: push rbx  (1 byte, executed)
    //   0x1000: cmc       (decoded as Invalid => side-exit; not executed)
    //
    // `Block.code_len` must cover only executed bytes, so the trace's code-version guards should
    // not include the second page.
    let entry = 0x0fff_u64;
    let mut bus = SimpleBus::new(0x3000);
    bus.load(entry, &[0x53]); // push rbx
    bus.load(0x1000, &[0xf5]); // cmc (unsupported by Tier-1 => Invalid)

    let func = build_function_from_x86(&bus, entry, 64, CfgBuildConfig::default());

    let env = RuntimeEnv::default();
    // `RuntimeEnv` starts with all page versions at 0; we only care about which pages are guarded.
    env.page_versions.set_version(0, 1);
    env.page_versions.set_version(1, 1);

    let mut profile = ProfileData::default();
    profile.block_counts.insert(func.entry, 10_000);

    let trace = TraceBuilder::new(
        &func,
        &profile,
        &env.page_versions,
        TraceConfig {
            hot_block_threshold: 1,
            max_blocks: 8,
            max_instrs: 256,
        },
    )
    .build_from(func.entry)
    .expect("trace should be hot");

    let guarded_pages: Vec<u64> = trace
        .ir
        .prologue
        .iter()
        .filter_map(|inst| match inst {
            Instr::GuardCodeVersion { page, .. } => Some(*page),
            _ => None,
        })
        .collect();

    assert_eq!(guarded_pages, vec![0]);
}
