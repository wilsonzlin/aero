#![cfg(not(target_arch = "wasm32"))]

use aero_x86::decoder::{decode, DecodeMode};
use aero_x86::inst::{AddressSize, Operand, SegmentReg};

fn decode_maskmov_mem(bytes: &[u8]) -> aero_x86::inst::MemoryOperand {
    let inst = decode(bytes, DecodeMode::Bits64, 0).expect("decode");
    inst.operands
        .iter()
        .find_map(|op| match op {
            Operand::Memory(mem) => Some(mem.clone()),
            _ => None,
        })
        .expect("expected memory operand")
}

#[test]
fn maskmov_includes_implicit_memory_operand() {
    // 66 0F F7 C1 => MASKMOVDQU xmm0, xmm1
    // Destination address is implicit: [DI/EDI/RDI].
    let bytes = [0x66, 0x0F, 0xF7, 0xC1];

    let inst = decode(&bytes, DecodeMode::Bits64, 0).expect("decode");
    assert_eq!(
        inst.operands
            .iter()
            .filter(|op| matches!(op, Operand::Memory(_)))
            .count(),
        1,
        "expected implicit memory operand for MASKMOV*"
    );

    let mem = decode_maskmov_mem(&bytes);
    assert_eq!(mem.addr_size, AddressSize::Bits64);
    assert_eq!(mem.base.map(|r| r.index), Some(7));
    assert_eq!(mem.index.map(|r| r.index), None);
    assert_eq!(mem.scale, 1);
    assert_eq!(mem.disp, 0);
    assert!(!mem.rip_relative);
    // No explicit segment override prefix => default segment selection (DS for DI/EDI/RDI).
    assert_eq!(mem.segment, None);
}

#[test]
fn ignored_segment_prefixes_do_not_clear_fs_gs_in_long_mode() {
    // In long mode, CS/DS/ES/SS overrides are accepted but ignored, and must not clear an earlier
    // FS/GS override (mirrors real hardware + iced-x86).
    //
    // 64    => FS override (effective)
    // 3E    => DS override (ignored in long mode; must NOT clear FS)
    // 66 0F F7 C1 => MASKMOVDQU xmm0, xmm1 (implicit mem operand at [RDI])
    let bytes = [0x64, 0x3E, 0x66, 0x0F, 0xF7, 0xC1];

    let inst = decode(&bytes, DecodeMode::Bits64, 0).expect("decode");
    assert_eq!(inst.prefixes.segment, Some(SegmentReg::FS));

    let mem = decode_maskmov_mem(&bytes);
    assert_eq!(mem.segment, Some(SegmentReg::FS));
}
