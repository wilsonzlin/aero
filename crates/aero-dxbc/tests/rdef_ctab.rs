use aero_dxbc::{parse_ctab_chunk, parse_rdef_chunk};

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[test]
fn parse_rdef_resource_bindings_minimal() {
    // Minimal RDEF-like chunk with a single texture bound at t3.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // cb count
    push_u32(&mut chunk, 0); // cb offset
    push_u32(&mut chunk, 1); // resource count
    push_u32(&mut chunk, 28); // resource offset (header size)
    push_u32(&mut chunk, 0); // shader model
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 0); // creator offset

    // Resource entry (32 bytes).
    push_u32(&mut chunk, 60); // name offset
    push_u32(&mut chunk, 0); // type
    push_u32(&mut chunk, 0); // return type
    push_u32(&mut chunk, 0); // dimension
    push_u32(&mut chunk, 0); // num samples
    push_u32(&mut chunk, 3); // bind point
    push_u32(&mut chunk, 1); // bind count
    push_u32(&mut chunk, 0); // flags

    chunk.extend_from_slice(b"tex0\0");

    let rdef = parse_rdef_chunk(&chunk).unwrap();
    assert_eq!(rdef.creator, None);
    assert_eq!(rdef.resources.len(), 1);
    assert_eq!(rdef.resources[0].name, "tex0");
    assert_eq!(rdef.resources[0].bind_point, 3);
    assert_eq!(rdef.resources[0].bind_count, 1);
}

#[test]
fn parse_ctab_constant_table_minimal() {
    // Minimal CTAB chunk with a single constant c0 and target string.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // size (ignored)
    push_u32(&mut chunk, 0); // creator offset
    push_u32(&mut chunk, 0); // version
    push_u32(&mut chunk, 1); // constant count
    push_u32(&mut chunk, 28); // constant info offset
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 48); // target offset (after entry)

    // Constant info entry (20 bytes).
    push_u32(&mut chunk, 55); // name offset (after target string)
    push_u16(&mut chunk, 0); // register set
    push_u16(&mut chunk, 0); // register index
    push_u16(&mut chunk, 1); // register count
    push_u16(&mut chunk, 0); // reserved
    push_u32(&mut chunk, 0); // type info offset
    push_u32(&mut chunk, 0); // default value offset

    chunk.extend_from_slice(b"ps_2_0\0"); // 7 bytes -> next offset 55
    chunk.extend_from_slice(b"C0\0");

    let ctab = parse_ctab_chunk(&chunk).unwrap();
    assert_eq!(ctab.creator, None);
    assert_eq!(ctab.target.as_deref(), Some("ps_2_0"));
    assert_eq!(ctab.constants.len(), 1);
    assert_eq!(ctab.constants[0].name, "C0");
    assert_eq!(ctab.constants[0].register_index, 0);
    assert_eq!(ctab.constants[0].register_count, 1);
}

