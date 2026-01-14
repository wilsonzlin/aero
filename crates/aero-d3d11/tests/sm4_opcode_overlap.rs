use std::collections::HashMap;

use aero_d3d11::sm4::opcode::*;

#[test]
fn sm4_integer_opcode_ids_do_not_overlap() {
    // Regression test: integer/bitwise ops, integer compares, and numeric conversion opcodes must
    // have distinct IDs. Overlaps can silently route instructions to the wrong decoder branch.
    let mut seen = HashMap::<u32, &'static str>::new();

    let record = |seen: &mut HashMap<u32, &'static str>, name: &'static str, opcode: u32| {
        if let Some(prev) = seen.insert(opcode, name) {
            panic!("{name} shares opcode ID {opcode:#x} with {prev}");
        }
    };

    // Integer / bitwise ops.
    record(&mut seen, "OPCODE_IADD", OPCODE_IADD);
    record(&mut seen, "OPCODE_ISUB", OPCODE_ISUB);
    record(&mut seen, "OPCODE_IMUL", OPCODE_IMUL);
    record(&mut seen, "OPCODE_AND", OPCODE_AND);
    record(&mut seen, "OPCODE_OR", OPCODE_OR);
    record(&mut seen, "OPCODE_XOR", OPCODE_XOR);
    record(&mut seen, "OPCODE_NOT", OPCODE_NOT);
    record(&mut seen, "OPCODE_ISHL", OPCODE_ISHL);
    record(&mut seen, "OPCODE_ISHR", OPCODE_ISHR);
    record(&mut seen, "OPCODE_USHR", OPCODE_USHR);

    // Integer compare ops.
    record(&mut seen, "OPCODE_IEQ", OPCODE_IEQ);
    record(&mut seen, "OPCODE_IGE", OPCODE_IGE);
    record(&mut seen, "OPCODE_ILT", OPCODE_ILT);
    record(&mut seen, "OPCODE_INE", OPCODE_INE);
    record(&mut seen, "OPCODE_ULT", OPCODE_ULT);
    record(&mut seen, "OPCODE_UGE", OPCODE_UGE);

    // Numeric conversions.
    record(&mut seen, "OPCODE_FTOI", OPCODE_FTOI);
    record(&mut seen, "OPCODE_FTOU", OPCODE_FTOU);
    record(&mut seen, "OPCODE_ITOF", OPCODE_ITOF);
    record(&mut seen, "OPCODE_UTOF", OPCODE_UTOF);
}

