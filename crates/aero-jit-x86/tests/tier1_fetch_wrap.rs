use std::collections::HashMap;

use aero_jit_x86::{discover_block_mode, BlockEndKind, BlockLimits, Tier1Bus};
use aero_x86::tier1::InstKind;

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
fn discover_block_mode_32bit_fetch_wraps_across_4gib_boundary() {
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

    let block = discover_block_mode(&bus, 0xffff_ffff, BlockLimits::default(), 32);
    assert_eq!(block.entry_rip, 0xffff_ffff);
    assert_eq!(block.insts.len(), 1);
    assert_eq!(block.insts[0].kind, InstKind::JmpRel { target: 0x1 });
    assert!(matches!(block.end_kind, BlockEndKind::Jmp));
}

#[test]
fn discover_block_mode_16bit_fetch_wraps_across_64k_boundary() {
    // Same idea as the 32-bit test, but across the 16-bit IP boundary.
    let mut bus = MapBus::default();
    bus.write_u8(0xffff, 0xeb);
    bus.write_u8(0x0000, 0x00);
    bus.write_u8(0x1_0000, 0x7f); // sentinel: should NOT be read in 16-bit mode

    let block = discover_block_mode(&bus, 0xffff, BlockLimits::default(), 16);
    assert_eq!(block.entry_rip, 0xffff);
    assert_eq!(block.insts.len(), 1);
    assert_eq!(block.insts[0].kind, InstKind::JmpRel { target: 0x1 });
    assert!(matches!(block.end_kind, BlockEndKind::Jmp));
}
