mod tier1_common;

use aero_jit_x86::tier2::ir::Terminator;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use tier1_common::SimpleBus;

#[test]
fn cfg_builder_linear_blocks() {
    // jmp +0
    // ud2
    let code = [
        0xeb, 0x00, // jmp 0x1002
        0x0f, 0x0b, // ud2
    ];
    let entry = 0x1000u64;

    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 64, CfgBuildConfig::default());
    assert_eq!(func.blocks.len(), 2);

    let b0 = func.find_block_by_rip(entry).unwrap();
    let b1 = func.find_block_by_rip(entry + 2).unwrap();

    match &func.block(b0).term {
        Terminator::Jump(t) => assert_eq!(*t, b1),
        other => panic!("expected Jump, got {other:?}"),
    }

    let block = func.block(b1);
    let exit_rip = match &block.term {
        Terminator::SideExit { exit_rip } => *exit_rip,
        other => panic!("expected SideExit terminator, got {other:?}"),
    };
    assert_eq!(
        exit_rip, block.start_rip,
        "side-exit rip should point at the unsupported instruction start (start=0x{:x}, exit=0x{:x})",
        block.start_rip, exit_rip
    );
}

#[test]
fn cfg_builder_conditional_branch() {
    // mov eax, 0
    // cmp eax, 0
    // jne target
    // fallthrough: ud2
    // target: ud2
    let code = [
        0xb8, 0x00, 0x00, 0x00, 0x00, // mov eax, 0
        0x83, 0xf8, 0x00, // cmp eax, 0
        0x75, 0x05, // jne +5 (target = 0x200f)
        0x0f, 0x0b, // ud2 (fallthrough @ 0x200a)
        0x90, 0x90, 0x90, // padding
        0x0f, 0x0b, // ud2 (target @ 0x200f)
    ];
    let entry = 0x2000u64;

    let mut bus = SimpleBus::new(0x3000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 64, CfgBuildConfig::default());
    assert_eq!(func.blocks.len(), 3);

    let head = func.find_block_by_rip(entry).unwrap();
    let fallthrough = func.find_block_by_rip(entry + 0x0a).unwrap();
    let target = func.find_block_by_rip(entry + 0x0f).unwrap();

    match &func.block(head).term {
        Terminator::Branch {
            then_bb, else_bb, ..
        } => {
            assert_eq!(*then_bb, target);
            assert_eq!(*else_bb, fallthrough);
        }
        other => panic!("expected Branch, got {other:?}"),
    }
}

#[test]
fn cfg_builder_loop_backedge() {
    // add eax, 1
    // cmp eax, 3
    // jne loop
    // exit: ud2
    let code = [
        0x83, 0xc0, 0x01, // add eax, 1
        0x83, 0xf8, 0x03, // cmp eax, 3
        0x75, 0xf8, // jne -8 (target = 0x3000)
        0x0f, 0x0b, // ud2 (exit @ 0x3008)
    ];
    let entry = 0x3000u64;

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 64, CfgBuildConfig::default());
    assert_eq!(func.blocks.len(), 2);

    let loop_bb = func.find_block_by_rip(entry).unwrap();
    let exit_bb = func.find_block_by_rip(entry + 0x8).unwrap();

    match &func.block(loop_bb).term {
        Terminator::Branch {
            then_bb, else_bb, ..
        } => {
            assert_eq!(*then_bb, loop_bb);
            assert_eq!(*else_bb, exit_bb);
        }
        other => panic!("expected Branch, got {other:?}"),
    }

    let block = func.block(exit_bb);
    let exit_rip = match &block.term {
        Terminator::SideExit { exit_rip } => *exit_rip,
        other => panic!("expected SideExit terminator, got {other:?}"),
    };
    assert_eq!(
        exit_rip, block.start_rip,
        "side-exit rip should point at the unsupported instruction start (start=0x{:x}, exit=0x{:x})",
        block.start_rip, exit_rip
    );
}
