#![no_main]

use aero_d3d11::sm4::opcode::{
    OPCODE_ADD, OPCODE_DP3, OPCODE_DP4, OPCODE_LD, OPCODE_LEN_SHIFT, OPCODE_MAD, OPCODE_MAX,
    OPCODE_MIN, OPCODE_MOV, OPCODE_MUL, OPCODE_RCP, OPCODE_RET, OPCODE_RSQ, OPCODE_SAMPLE,
    OPERAND_COMPONENT_SELECTION_SHIFT, OPERAND_EXTENDED_BIT, OPERAND_INDEX0_REP_SHIFT,
    OPERAND_INDEX1_REP_SHIFT, OPERAND_INDEX2_REP_SHIFT, OPERAND_INDEX_DIMENSION_0D,
    OPERAND_INDEX_DIMENSION_1D, OPERAND_INDEX_DIMENSION_2D, OPERAND_INDEX_DIMENSION_SHIFT,
    OPERAND_INDEX_REP_IMMEDIATE32, OPERAND_SELECTION_MODE_SHIFT, OPERAND_SEL_MASK,
    OPERAND_SEL_SELECT1, OPERAND_SEL_SWIZZLE, OPERAND_TYPE_CONSTANT_BUFFER,
    OPERAND_TYPE_IMMEDIATE32, OPERAND_TYPE_INPUT, OPERAND_TYPE_OUTPUT, OPERAND_TYPE_RESOURCE,
    OPERAND_TYPE_SAMPLER, OPERAND_TYPE_SHIFT, OPERAND_TYPE_TEMP,
};
use aero_d3d11::sm4::{FOURCC_SHDR, FOURCC_SHEX};
use aero_dxbc::DxbcFile;
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological work in DXBC chunk-table validation and SM4 token
/// decoding. We keep this aligned with the other fuzzers in this repository (1 MiB).
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024;

/// Additional cap for the "raw input is a DXBC container" path. Large random blobs can trick the
/// DXBC header into indicating a huge chunk_count, which makes `DxbcFile::parse` do a lot of
/// validation work. We keep a smaller cap here to maintain fuzz throughput.
const MAX_RAW_DXBC_SIZE_BYTES: usize = 256 * 1024;

/// Cap the shader chunk payload size before calling `Sm4Program::parse_from_dxbc` to avoid
/// allocating huge `Vec<u32>` token buffers.
const MAX_SHADER_CHUNK_BYTES: usize = 256 * 1024;

/// Cap the parsed SM4/SM5 token stream length (DWORDs) before attempting IR decoding / translation.
///
/// The DXBC chunk-size cap already limits the token stream length, but keeping a smaller bound here
/// avoids spending a lot of time decoding/formatting extremely long (but still in-bounds) programs.
const MAX_PROGRAM_TOKENS_DWORDS: usize = 16 * 1024;

/// Cap the size of the synthetic shader token stream we generate from fuzzer input.
const MAX_SYNTH_INSTRUCTIONS: usize = 128;

/// Cap the number of synthetic signature parameters we generate (keeps signature chunk parsing
/// + WGSL struct generation bounded).
const MAX_SYNTH_SIGNATURE_PARAMS: usize = 16;

/// Cap IR sizes before attempting WGSL generation to avoid pathological string allocations.
const MAX_TRANSLATE_DECLS: usize = 4 * 1024;
const MAX_TRANSLATE_INSTRUCTIONS: usize = 4 * 1024;

#[derive(Clone, Copy)]
struct SigParam<'a> {
    semantic_name: &'a [u8],
    semantic_index: u32,
    system_value_type: u32,
    component_type: u32,
    register: u32,
    mask: u8,
    read_write_mask: u8,
    stream: u8,
}

fn build_signature_chunk_v0(params: &[SigParam<'_>]) -> Vec<u8> {
    // v0 signature layout:
    // - header: param_count (u32), param_offset (u32)
    // - entries: 24 bytes each
    // - string table: null-terminated semantic names
    const HEADER_LEN: usize = 8;
    const ENTRY_LEN: usize = 24;

    let param_count = params.len().min(MAX_SYNTH_SIGNATURE_PARAMS);
    let table_offset = HEADER_LEN;
    let table_len = ENTRY_LEN * param_count;
    let mut out = Vec::with_capacity(HEADER_LEN + table_len + 64);

    out.extend_from_slice(&(param_count as u32).to_le_bytes());
    out.extend_from_slice(&(table_offset as u32).to_le_bytes());

    // Reserve entry table space.
    out.resize(HEADER_LEN + table_len, 0);

    // Emit semantic names after the entry table.
    let mut string_base = HEADER_LEN + table_len;
    for (idx, p) in params.iter().take(param_count).enumerate() {
        let entry_off = HEADER_LEN + idx * ENTRY_LEN;
        let name_off = string_base as u32;

        // Semantic name bytes must be valid UTF-8 for the signature parser.
        let name = if core::str::from_utf8(p.semantic_name).is_ok() {
            p.semantic_name
        } else {
            b"BADSEM"
        };

        // entry[0..4] = semantic_name_offset
        out[entry_off..entry_off + 4].copy_from_slice(&name_off.to_le_bytes());
        out[entry_off + 4..entry_off + 8].copy_from_slice(&p.semantic_index.to_le_bytes());
        out[entry_off + 8..entry_off + 12].copy_from_slice(&p.system_value_type.to_le_bytes());
        out[entry_off + 12..entry_off + 16].copy_from_slice(&p.component_type.to_le_bytes());
        out[entry_off + 16..entry_off + 20].copy_from_slice(&p.register.to_le_bytes());

        let packed =
            (p.mask as u32) | ((p.read_write_mask as u32) << 8) | ((p.stream as u32) << 16);
        out[entry_off + 20..entry_off + 24].copy_from_slice(&packed.to_le_bytes());

        out.extend_from_slice(name);
        out.push(0);
        string_base = out.len();
    }

    out
}

#[derive(Clone)]
struct DxbcChunkOwned {
    fourcc: [u8; 4],
    data: Vec<u8>,
}

fn build_dxbc_container(chunks: &[DxbcChunkOwned]) -> Vec<u8> {
    // DXBC header layout:
    // - magic: "DXBC"
    // - checksum: 16 bytes
    // - reserved: u32
    // - total_size: u32
    // - chunk_count: u32
    // - chunk_offsets: chunk_count * u32
    const HEADER_LEN: usize = 4 + 16 + 4 + 4 + 4;

    let chunk_count = chunks.len().min(16);
    let mut out = Vec::with_capacity(HEADER_LEN + chunk_count * 4);

    out.extend_from_slice(b"DXBC");
    out.extend_from_slice(&[0u8; 16]); // checksum (ignored by parser)
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&0u32.to_le_bytes()); // total_size placeholder
    out.extend_from_slice(&(chunk_count as u32).to_le_bytes());

    let offset_table_off = out.len();
    out.resize(offset_table_off + chunk_count * 4, 0);

    let mut offsets = Vec::<u32>::with_capacity(chunk_count);
    for chunk in chunks.iter().take(chunk_count) {
        let off = out.len() as u32;
        offsets.push(off);
        out.extend_from_slice(&chunk.fourcc);
        out.extend_from_slice(&(chunk.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&chunk.data);
    }

    // total_size
    let total_size = out.len() as u32;
    out[24..28].copy_from_slice(&total_size.to_le_bytes());

    // offsets
    for (i, off) in offsets.iter().enumerate() {
        let base = offset_table_off + i * 4;
        out[base..base + 4].copy_from_slice(&off.to_le_bytes());
    }

    out
}

fn pack_opcode(opcode: u32, len_dwords: usize) -> u32 {
    opcode | ((len_dwords as u32) << OPCODE_LEN_SHIFT)
}

fn pack_operand_token(
    num_components: u32,
    selection_mode: u32,
    ty: u32,
    component_sel: u32,
    index_dim: u32,
    idx0_rep: u32,
    idx1_rep: u32,
    idx2_rep: u32,
    extended: bool,
) -> u32 {
    let mut token = (num_components & 0x3)
        | ((selection_mode & 0x3) << OPERAND_SELECTION_MODE_SHIFT)
        | ((ty & 0xff) << OPERAND_TYPE_SHIFT)
        | ((component_sel & 0xff) << OPERAND_COMPONENT_SELECTION_SHIFT)
        | ((index_dim & 0x3) << OPERAND_INDEX_DIMENSION_SHIFT)
        | ((idx0_rep & 0x7) << OPERAND_INDEX0_REP_SHIFT)
        | ((idx1_rep & 0x7) << OPERAND_INDEX1_REP_SHIFT)
        | ((idx2_rep & 0x7) << OPERAND_INDEX2_REP_SHIFT);
    if extended {
        token |= OPERAND_EXTENDED_BIT;
    }
    token
}

fn encode_reg_operand(ty: u32, index: u32, selection_mode: u32, component_sel: u32) -> [u32; 2] {
    [
        pack_operand_token(
            /*num_components=*/ 0,
            selection_mode,
            ty,
            component_sel,
            OPERAND_INDEX_DIMENSION_1D,
            OPERAND_INDEX_REP_IMMEDIATE32,
            0,
            0,
            /*extended=*/ false,
        ),
        index,
    ]
}

fn encode_cbuffer_operand(
    slot: u32,
    reg: u32,
    selection_mode: u32,
    component_sel: u32,
) -> [u32; 3] {
    [
        pack_operand_token(
            /*num_components=*/ 0,
            selection_mode,
            OPERAND_TYPE_CONSTANT_BUFFER,
            component_sel,
            OPERAND_INDEX_DIMENSION_2D,
            OPERAND_INDEX_REP_IMMEDIATE32,
            OPERAND_INDEX_REP_IMMEDIATE32,
            0,
            /*extended=*/ false,
        ),
        slot,
        reg,
    ]
}

fn encode_imm_vec4(vals: [u32; 4]) -> [u32; 5] {
    [
        pack_operand_token(
            /*num_components=*/ 2,
            OPERAND_SEL_MASK,
            OPERAND_TYPE_IMMEDIATE32,
            /*component_sel=*/ 0,
            OPERAND_INDEX_DIMENSION_0D,
            0,
            0,
            0,
            /*extended=*/ false,
        ),
        vals[0],
        vals[1],
        vals[2],
        vals[3],
    ]
}

fn encode_swizzle_from_u8(v: u8) -> u32 {
    // 2-bit components packed into 8 bits (x,y,z,w), matching `decode_swizzle`.
    v as u32
}

fn gen_src_operand(u: &mut Unstructured<'_>, allow_input: bool) -> (Vec<u32>, u32) {
    // Returns (operand_tokens, max_input_reg_used).
    let kind = u.arbitrary::<u8>().unwrap_or(0) % 5;
    match kind {
        // Temp reg.
        0 => {
            let idx = (u.arbitrary::<u8>().unwrap_or(0) % 16) as u32;
            let sel = match u.arbitrary::<u8>().unwrap_or(0) % 3 {
                0 => (OPERAND_SEL_MASK, 0xF),
                1 => (
                    OPERAND_SEL_SWIZZLE,
                    encode_swizzle_from_u8(u.arbitrary().unwrap_or(0)),
                ),
                _ => (
                    OPERAND_SEL_SELECT1,
                    (u.arbitrary::<u8>().unwrap_or(0) & 3) as u32,
                ),
            };
            (
                encode_reg_operand(OPERAND_TYPE_TEMP, idx, sel.0, sel.1).to_vec(),
                0,
            )
        }
        // Constant buffer.
        1 => {
            let slot = (u.arbitrary::<u8>().unwrap_or(0) % 8) as u32;
            let reg = (u.arbitrary::<u8>().unwrap_or(0) % 32) as u32;
            (
                encode_cbuffer_operand(slot, reg, OPERAND_SEL_MASK, 0xF).to_vec(),
                0,
            )
        }
        // Immediate vec4.
        2 => {
            let vals = [
                u.arbitrary::<u32>().unwrap_or(0),
                u.arbitrary::<u32>().unwrap_or(0),
                u.arbitrary::<u32>().unwrap_or(0),
                u.arbitrary::<u32>().unwrap_or(0),
            ];
            (encode_imm_vec4(vals).to_vec(), 0)
        }
        // Input reg (restricted to v0/v1 so we can declare signatures).
        3 if allow_input => {
            let idx = (u.arbitrary::<u8>().unwrap_or(0) & 1) as u32;
            let sel = match u.arbitrary::<u8>().unwrap_or(0) % 3 {
                0 => (OPERAND_SEL_MASK, 0xF),
                1 => (
                    OPERAND_SEL_SWIZZLE,
                    encode_swizzle_from_u8(u.arbitrary().unwrap_or(0)),
                ),
                _ => (
                    OPERAND_SEL_SELECT1,
                    (u.arbitrary::<u8>().unwrap_or(0) & 3) as u32,
                ),
            };
            (
                encode_reg_operand(OPERAND_TYPE_INPUT, idx, sel.0, sel.1).to_vec(),
                idx + 1,
            )
        }
        // Output reg (rare, but allowed as a source operand).
        _ => {
            let idx = (u.arbitrary::<u8>().unwrap_or(0) % 4) as u32;
            (
                encode_reg_operand(OPERAND_TYPE_OUTPUT, idx, OPERAND_SEL_MASK, 0xF).to_vec(),
                0,
            )
        }
    }
}

fn gen_dst_operand(u: &mut Unstructured<'_>, ty: u32, max_index: u8) -> Vec<u32> {
    let idx = (u.arbitrary::<u8>().unwrap_or(0) % max_index) as u32;
    let mut mask = (u.arbitrary::<u8>().unwrap_or(0) & 0xF) as u32;
    // `emit_write_masked` rejects mask 0; keep it non-zero so we hit more translation paths.
    if mask == 0 {
        mask = 0xF;
    }
    encode_reg_operand(ty, idx, OPERAND_SEL_MASK, mask).to_vec()
}

fn gen_sm4_tokens(u: &mut Unstructured<'_>, is_vertex: bool, major: u8) -> (Vec<u32>, u32) {
    // Returns (tokens, max_input_reg_count).
    let ty = if is_vertex { 1u32 } else { 0u32 };
    let version = ((ty as u32) << 16) | ((major as u32) << 4) | 0u32;

    let mut tokens = Vec::<u32>::new();
    tokens.push(version);
    tokens.push(0); // declared_len placeholder

    // Instruction count is derived from fuzzer data but bounded.
    let inst_count = (u.arbitrary::<u8>().unwrap_or(0) as usize) % (MAX_SYNTH_INSTRUCTIONS + 1);

    let mut max_input_reg_count = 0u32;

    // Seed: r0 = v0 (keeps shaders "connected" to signatures).
    {
        let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 8);
        let idx = (u.arbitrary::<u8>().unwrap_or(0) & 1) as u32;
        max_input_reg_count = max_input_reg_count.max(idx + 1);
        let sel = match u.arbitrary::<u8>().unwrap_or(0) % 3 {
            0 => (OPERAND_SEL_MASK, 0xF),
            1 => (
                OPERAND_SEL_SWIZZLE,
                encode_swizzle_from_u8(u.arbitrary().unwrap_or(0)),
            ),
            _ => (
                OPERAND_SEL_SELECT1,
                (u.arbitrary::<u8>().unwrap_or(0) & 3) as u32,
            ),
        };
        let src = encode_reg_operand(OPERAND_TYPE_INPUT, idx, sel.0, sel.1).to_vec();
        let len = 1 + dst.len() + src.len();
        tokens.push(pack_opcode(OPCODE_MOV, len));
        tokens.extend(dst.into_iter());
        tokens.extend(src.into_iter());
    }

    // Additional random instructions operating on temps.
    for _ in 0..inst_count {
        let opcode_sel = u.arbitrary::<u8>().unwrap_or(0) % 12;

        match opcode_sel {
            0 => {
                // mov r#, src
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (src, used) = gen_src_operand(u, /*allow_input=*/ true);
                max_input_reg_count = max_input_reg_count.max(used);
                let len = 1 + dst.len() + src.len();
                tokens.push(pack_opcode(OPCODE_MOV, len));
                tokens.extend(dst);
                tokens.extend(src);
            }
            1 => {
                // add r#, a, b
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (a, used_a) = gen_src_operand(u, true);
                let (b, used_b) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used_a).max(used_b);
                let len = 1 + dst.len() + a.len() + b.len();
                tokens.push(pack_opcode(OPCODE_ADD, len));
                tokens.extend(dst);
                tokens.extend(a);
                tokens.extend(b);
            }
            2 => {
                // mul r#, a, b
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (a, used_a) = gen_src_operand(u, true);
                let (b, used_b) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used_a).max(used_b);
                let len = 1 + dst.len() + a.len() + b.len();
                tokens.push(pack_opcode(OPCODE_MUL, len));
                tokens.extend(dst);
                tokens.extend(a);
                tokens.extend(b);
            }
            3 => {
                // mad r#, a, b, c
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (a, used_a) = gen_src_operand(u, true);
                let (b, used_b) = gen_src_operand(u, true);
                let (c, used_c) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used_a).max(used_b).max(used_c);
                let len = 1 + dst.len() + a.len() + b.len() + c.len();
                tokens.push(pack_opcode(OPCODE_MAD, len));
                tokens.extend(dst);
                tokens.extend(a);
                tokens.extend(b);
                tokens.extend(c);
            }
            4 => {
                // dp3 r#, a, b
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (a, used_a) = gen_src_operand(u, true);
                let (b, used_b) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used_a).max(used_b);
                let len = 1 + dst.len() + a.len() + b.len();
                tokens.push(pack_opcode(OPCODE_DP3, len));
                tokens.extend(dst);
                tokens.extend(a);
                tokens.extend(b);
            }
            5 => {
                // dp4 r#, a, b
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (a, used_a) = gen_src_operand(u, true);
                let (b, used_b) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used_a).max(used_b);
                let len = 1 + dst.len() + a.len() + b.len();
                tokens.push(pack_opcode(OPCODE_DP4, len));
                tokens.extend(dst);
                tokens.extend(a);
                tokens.extend(b);
            }
            6 => {
                // min r#, a, b
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (a, used_a) = gen_src_operand(u, true);
                let (b, used_b) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used_a).max(used_b);
                let len = 1 + dst.len() + a.len() + b.len();
                tokens.push(pack_opcode(OPCODE_MIN, len));
                tokens.extend(dst);
                tokens.extend(a);
                tokens.extend(b);
            }
            7 => {
                // max r#, a, b
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (a, used_a) = gen_src_operand(u, true);
                let (b, used_b) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used_a).max(used_b);
                let len = 1 + dst.len() + a.len() + b.len();
                tokens.push(pack_opcode(OPCODE_MAX, len));
                tokens.extend(dst);
                tokens.extend(a);
                tokens.extend(b);
            }
            8 => {
                // rcp r#, src
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (src, used) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used);
                let len = 1 + dst.len() + src.len();
                tokens.push(pack_opcode(OPCODE_RCP, len));
                tokens.extend(dst);
                tokens.extend(src);
            }
            9 => {
                // rsq r#, src
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (src, used) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used);
                let len = 1 + dst.len() + src.len();
                tokens.push(pack_opcode(OPCODE_RSQ, len));
                tokens.extend(dst);
                tokens.extend(src);
            }
            10 => {
                // sample r#, coord, t#, s#
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (coord, used) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used);
                let t_slot = (u.arbitrary::<u8>().unwrap_or(0) % 4) as u32;
                let s_slot = (u.arbitrary::<u8>().unwrap_or(0) % 4) as u32;
                let texture =
                    encode_reg_operand(OPERAND_TYPE_RESOURCE, t_slot, OPERAND_SEL_MASK, 0xF);
                let sampler =
                    encode_reg_operand(OPERAND_TYPE_SAMPLER, s_slot, OPERAND_SEL_MASK, 0xF);
                let len = 1 + dst.len() + coord.len() + texture.len() + sampler.len();
                tokens.push(pack_opcode(OPCODE_SAMPLE, len));
                tokens.extend(dst);
                tokens.extend(coord);
                tokens.extend(texture);
                tokens.extend(sampler);
            }
            _ => {
                // ld r#, coord, t#
                let dst = gen_dst_operand(u, OPERAND_TYPE_TEMP, 16);
                let (coord, used) = gen_src_operand(u, true);
                max_input_reg_count = max_input_reg_count.max(used);
                let t_slot = (u.arbitrary::<u8>().unwrap_or(0) % 4) as u32;
                let texture =
                    encode_reg_operand(OPERAND_TYPE_RESOURCE, t_slot, OPERAND_SEL_MASK, 0xF);
                let len = 1 + dst.len() + coord.len() + texture.len();
                tokens.push(pack_opcode(OPCODE_LD, len));
                tokens.extend(dst);
                tokens.extend(coord);
                tokens.extend(texture);
            }
        }

        // Keep the token stream bounded even if the fuzzer produces pathological operands.
        if tokens.len() > (2 + MAX_SYNTH_INSTRUCTIONS * 20) {
            break;
        }
    }

    // Final: mov o0, r0 (or another temp) then ret.
    {
        let dst = encode_reg_operand(OPERAND_TYPE_OUTPUT, 0, OPERAND_SEL_MASK, 0xF).to_vec();
        let src_idx = (u.arbitrary::<u8>().unwrap_or(0) % 16) as u32;
        let src = encode_reg_operand(OPERAND_TYPE_TEMP, src_idx, OPERAND_SEL_MASK, 0xF).to_vec();
        let len = 1 + dst.len() + src.len();
        tokens.push(pack_opcode(OPCODE_MOV, len));
        tokens.extend(dst);
        tokens.extend(src);
    }

    tokens.push(pack_opcode(OPCODE_RET, 1));

    // Fix up declared length.
    tokens[1] = tokens.len() as u32;
    (tokens, max_input_reg_count.max(1))
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn build_synthetic_dxbc(data: &[u8]) -> Vec<u8> {
    let mut u = Unstructured::new(data);
    let cfg = u.arbitrary::<u8>().unwrap_or(0);
    let is_vertex = (cfg & 1) == 0;
    let major = if (cfg & 2) == 0 { 4u8 } else { 5u8 };

    let (tokens, max_input_reg_count) = gen_sm4_tokens(&mut u, is_vertex, major);
    let shader_bytes = tokens_to_bytes(&tokens);

    // Synthetic signatures: declare v0..v{max_input_reg_count-1} (capped) and the required output.
    let mut sig_in = Vec::<SigParam>::new();
    let mut sig_out = Vec::<SigParam>::new();

    let max_inputs = (max_input_reg_count as usize).min(MAX_SYNTH_SIGNATURE_PARAMS);
    for reg in 0..max_inputs {
        let mut mask = (u.arbitrary::<u8>().unwrap_or(0) & 0xF) as u8;
        if mask == 0 {
            mask = 0xF;
        }
        // Keep semantics simple and ASCII; these are used by sys-value heuristics.
        let semantic_name: &[u8] = if is_vertex {
            &b"POSITION"[..]
        } else {
            &b"SV_Position"[..]
        };
        sig_in.push(SigParam {
            semantic_name,
            semantic_index: reg as u32,
            system_value_type: 0,
            component_type: 0,
            register: reg as u32,
            mask,
            read_write_mask: 0xF,
            stream: 0,
        });
    }

    // Output signature: o0 is always either VS position or PS target0 in our synthetic programs.
    let out_semantic: &[u8] = if is_vertex {
        &b"SV_Position"[..]
    } else {
        &b"SV_Target"[..]
    };
    let out_sys_value = if is_vertex { 1u32 } else { 64u32 };
    sig_out.push(SigParam {
        semantic_name: out_semantic,
        semantic_index: 0,
        system_value_type: out_sys_value,
        component_type: 0,
        register: 0,
        mask: 0xF,
        read_write_mask: 0xF,
        stream: 0,
    });

    let shader_fourcc = if major >= 5 {
        FOURCC_SHEX.0
    } else {
        FOURCC_SHDR.0
    };

    let chunks = vec![
        DxbcChunkOwned {
            fourcc: *b"ISGN",
            data: build_signature_chunk_v0(&sig_in),
        },
        DxbcChunkOwned {
            fourcc: *b"OSGN",
            data: build_signature_chunk_v0(&sig_out),
        },
        DxbcChunkOwned {
            fourcc: shader_fourcc,
            data: shader_bytes,
        },
    ];

    build_dxbc_container(&chunks)
}

fn fuzz_translate_dxbc_bytes(bytes: &[u8], allow_bootstrap: bool) {
    let Ok(dxbc) = DxbcFile::parse(bytes) else {
        return;
    };

    // Cap shader chunk size before tokenization.
    let shader_chunk = dxbc
        .get_chunk(FOURCC_SHEX)
        .or_else(|| dxbc.get_chunk(FOURCC_SHDR))
        .or_else(|| dxbc.find_first_shader_chunk());
    if shader_chunk
        .as_ref()
        .is_some_and(|c| c.data.len() > MAX_SHADER_CHUNK_BYTES)
    {
        return;
    }

    let Ok(program) = aero_d3d11::sm4::Sm4Program::parse_from_dxbc(&dxbc) else {
        return;
    };

    // Size cap: avoid doing expensive decode/translation work on very long token streams.
    if program.tokens.len() > MAX_PROGRAM_TOKENS_DWORDS {
        return;
    }

    // The bootstrap translator is token-stream driven and historically assumed register indices are
    // "reasonable" (e.g. it may emit `struct` fields for `0..=max_vreg`). Keep it behind a gate so
    // the raw-DXBC fuzzing path can't trigger pathological allocations. The synthetic DXBC path
    // uses bounded register indices and is safe to exercise here.
    if allow_bootstrap {
        let _ = aero_d3d11::translate_sm4_to_wgsl_bootstrap(&program);
    }

    let Ok(module) = aero_d3d11::sm4::decode_program(&program) else {
        return;
    };

    if module.decls.len() > MAX_TRANSLATE_DECLS
        || module.instructions.len() > MAX_TRANSLATE_INSTRUCTIONS
    {
        return;
    }

    let signatures = aero_d3d11::parse_signatures(&dxbc).unwrap_or_default();
    let _ = aero_d3d11::translate_sm4_module_to_wgsl(&dxbc, &module, &signatures);
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // 1) Raw path: treat fuzzer input as a full DXBC container (covers container parsing and
    //    chunk validation against hostile offsets).
    if data.len() <= MAX_RAW_DXBC_SIZE_BYTES {
        fuzz_translate_dxbc_bytes(data, /*allow_bootstrap=*/ false);
    }

    // 2) Synthetic path: wrap the fuzzer bytes in a minimal, structurally valid DXBC container
    //    with signature chunks so we can reach deeper SM4 decoding + WGSL translation logic.
    let synth = build_synthetic_dxbc(data);
    fuzz_translate_dxbc_bytes(&synth, /*allow_bootstrap=*/ true);
});
