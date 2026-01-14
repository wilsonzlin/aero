mod tier1_common;

use std::collections::HashMap;

use aero_cpu_core::jit::runtime::PageVersionTracker;
use aero_jit_x86::tier2::ir::Instr;
use aero_jit_x86::tier2::profile::{ProfileData, TraceConfig};
use aero_jit_x86::tier2::trace::TraceBuilder;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_jit_x86::Tier1Bus;
use tier1_common::SimpleBus;

#[derive(Default)]
struct MapBus {
    mem: HashMap<u64, u8>,
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
fn tier2_code_version_guard_ignores_trailing_invalid_page() {
    // Place an executed instruction at the last byte of a page, followed by an unsupported opcode
    // on the next page:
    //   0x0FFF: push rbx          (1 byte, executed)
    //   0x1000: <unsupported op>  (decoded as Invalid; Tier-1/Tier-2 side-exit at 0x1000; not executed)
    //
    // Tier-2 `Block::code_len` is expected to cover only executed bytes, so trace code-version
    // guards should not include the second page.
    for bitness in [16u32, 32, 64] {
        let entry = 0x0fff_u64;
        let mut bus = SimpleBus::new(0x3000);
        bus.load(entry, &[0x53]); // push rbx/bx/ebx
        let invalid = tier1_common::pick_invalid_opcode(bitness);
        bus.load(0x1000, &[invalid]); // unsupported opcode (decoded as Invalid by Tier-1)

        let func = build_function_from_x86(&bus, entry, bitness, CfgBuildConfig::default());
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
        assert_eq!(
            guarded_pages,
            vec![entry_page],
            "unexpected guarded pages for bitness={bitness}"
        );
    }
}

#[test]
fn tier2_code_version_guard_wraps_ip_across_bitness_boundary() {
    // Exercise the corner case where a block's *executed* code bytes span the architectural IP wrap
    // boundary in 16/32-bit mode. Code-version guards must include the wrapped page (page 0),
    // otherwise self-modifying writes to the wrapped bytes would not invalidate Tier-2 traces.
    //
    // Guest bytes (for both bitness=16 and bitness=32):
    //   <boundary-2>: 31 C0    xor (e)ax, (e)ax   (2 bytes)
    //   0x0000:      40       inc (e)ax          (1 byte)
    //   0x0001:      <invalid>
    //
    // The Invalid instruction is not executed (Tier-1/Tier-2 side-exit at its RIP), so `code_len`
    // covers only the first 3 bytes, which wrap across the boundary.
    for (bitness, entry_rip) in [(16u32, 0xfffeu64), (32u32, 0xffff_fffeu64)] {
        let mut bus = MapBus::default();
        bus.write_u8(entry_rip, 0x31);
        bus.write_u8(entry_rip.wrapping_add(1), 0xc0);
        bus.write_u8(0x0000, 0x40);
        bus.write_u8(0x0001, tier1_common::pick_invalid_opcode(bitness));

        let func = build_function_from_x86(&bus, entry_rip, bitness, CfgBuildConfig::default());
        assert_eq!(func.blocks.len(), 1, "expected a single-block CFG");
        assert_eq!(func.block(func.entry).code_len, 3);

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

        let guarded_pages: Vec<u64> = trace
            .ir
            .prologue
            .iter()
            .filter_map(|inst| match inst {
                Instr::GuardCodeVersion { page, .. } => Some(*page),
                _ => None,
            })
            .collect();

        let high_page = entry_rip >> aero_jit_x86::PAGE_SHIFT;
        assert_eq!(
            guarded_pages,
            vec![0, high_page],
            "unexpected guarded pages for bitness={bitness}"
        );
    }
}
