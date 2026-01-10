use aero_cpu_decoder::{decode_one, DecodeMode};
use iced_x86::Register;

#[test]
fn decodes_rip_relative_memory_operand() {
    // 48 8B 05 78 56 34 12  => mov rax, qword ptr [rip+0x12345678]
    let bytes = [0x48, 0x8B, 0x05, 0x78, 0x56, 0x34, 0x12];
    let decoded = decode_one(DecodeMode::Bits64, 0x1000, &bytes).expect("decode");
    let ins = decoded.instruction;
    assert_eq!(ins.op_count(), 2);
    assert_eq!(ins.memory_base(), Register::RIP);
    assert_eq!(ins.memory_index(), Register::None);
    // iced-x86 returns the *effective* IP-relative address in `memory_displacement64()`.
    let expected = (0x1000u64 + ins.len() as u64).wrapping_add(0x12345678);
    assert_eq!(ins.memory_displacement64(), expected);
}

#[test]
fn decodes_sib_scaled_index() {
    // 48 8B 84 8B 78 56 34 12
    // mov rax, qword ptr [rbx+rcx*4+0x12345678]
    let bytes = [0x48, 0x8B, 0x84, 0x8B, 0x78, 0x56, 0x34, 0x12];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    let ins = decoded.instruction;
    assert_eq!(ins.memory_base(), Register::RBX);
    assert_eq!(ins.memory_index(), Register::RCX);
    assert_eq!(ins.memory_index_scale(), 4);
    assert_eq!(ins.memory_displacement64(), 0x12345678);
}
