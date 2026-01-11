mod tier1_common;

use aero_jit_x86::tier1::BlockLimits;
use aero_jit_x86::tier2::build_t2_function;
use aero_jit_x86::tier2::ir::Terminator;
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

    let func = build_t2_function(&bus, entry, BlockLimits::default());
    assert_eq!(func.blocks.len(), 2);

    let b0 = func.find_block_by_rip(entry).unwrap();
    let b1 = func.find_block_by_rip(entry + 2).unwrap();

    match &func.block(b0).term {
        Terminator::Jump(t) => assert_eq!(*t, b1),
        other => panic!("expected Jump, got {other:?}"),
    }

    match &func.block(b1).term {
        Terminator::SideExit { exit_rip } => {
            assert!(
                *exit_rip > func.block(b1).start_rip,
                "side-exit rip must advance (start=0x{:x}, exit=0x{:x})",
                func.block(b1).start_rip,
                exit_rip
            );
        }
        other => panic!("expected SideExit, got {other:?}"),
    }
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

    let func = build_t2_function(&bus, entry, BlockLimits::default());
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

    let func = build_t2_function(&bus, entry, BlockLimits::default());
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

    match &func.block(exit_bb).term {
        Terminator::SideExit { exit_rip } => {
            assert!(
                *exit_rip > func.block(exit_bb).start_rip,
                "side-exit rip must advance (start=0x{:x}, exit=0x{:x})",
                func.block(exit_bb).start_rip,
                exit_rip
            );
        }
        other => panic!("expected SideExit, got {other:?}"),
    }
}
