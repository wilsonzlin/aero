mod tier1_common;

use std::collections::HashMap;

use aero_jit_x86::tier2::ir::Terminator;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_jit_x86::Tier1Bus;

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
fn tier2_cfg_builder_32bit_fetch_wraps_across_4gib_boundary() {
    // Bytes at the 4GiB wrap boundary:
    //   0xFFFF_FFFF: EB          jmp short +0
    //   0x0000_0000: 00
    //
    // If instruction fetch doesn't wrap to 32-bit, the decoder would incorrectly read the rel8
    // immediate from 0x1_0000_0000 instead.
    let mut bus = MapBus::default();
    bus.write_u8(0xffff_ffff, 0xeb);
    bus.write_u8(0x0000_0000, 0x00);
    bus.write_u8(0x1_0000_0000, 0x7f); // sentinel: should NOT be read in 32-bit mode
    bus.write_u8(0x1, tier1_common::pick_invalid_opcode(32)); // ensure the jump target terminates

    let func = build_function_from_x86(&bus, 0xffff_ffff, 32, CfgBuildConfig::default());
    let entry = func.find_block_by_rip(0xffff_ffff).expect("entry block");
    let target = func.find_block_by_rip(0x1).expect("target block");

    match &func.block(entry).term {
        Terminator::Jump(t) => assert_eq!(*t, target),
        other => panic!("expected Jump terminator, got {other:?}"),
    }
}

#[test]
fn tier2_cfg_builder_16bit_fetch_wraps_across_64k_boundary() {
    // Same idea as the 32-bit test, but across the 16-bit IP boundary.
    let mut bus = MapBus::default();
    bus.write_u8(0xffff, 0xeb);
    bus.write_u8(0x0000, 0x00);
    bus.write_u8(0x1_0000, 0x7f); // sentinel: should NOT be read in 16-bit mode
    bus.write_u8(0x1, tier1_common::pick_invalid_opcode(16)); // ensure the jump target terminates

    let func = build_function_from_x86(&bus, 0xffff, 16, CfgBuildConfig::default());
    let entry = func.find_block_by_rip(0xffff).expect("entry block");
    let target = func.find_block_by_rip(0x1).expect("target block");

    match &func.block(entry).term {
        Terminator::Jump(t) => assert_eq!(*t, target),
        other => panic!("expected Jump terminator, got {other:?}"),
    }
}

