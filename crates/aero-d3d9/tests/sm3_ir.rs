use aero_d3d9::dxbc;
use aero_d3d9::sm3::decode::{SrcModifier, Swizzle};
use aero_d3d9::sm3::ir::{Block, Cond, RegFile, RegRef, ShaderIr, Src, Stmt};
use aero_d3d9::sm3::types::{ShaderStage, ShaderVersion};
use aero_d3d9::sm3::{build_ir, decode_u8_le_bytes, generate_wgsl, verify_ir};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC as DxbcFourCC};

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

fn opcode_token(op: u16, operand_count: u8) -> u32 {
    // D3D9 SM2/SM3 encodes the *total* instruction length in tokens (including the opcode token)
    // in bits 24..27.
    (op as u32) | (((operand_count as u32) + 1) << 24)
}

fn reg_token(regtype: u8, index: u32) -> u32 {
    let low3 = (regtype as u32) & 0x7;
    let high2 = (regtype as u32) & 0x18;
    0x8000_0000 | (low3 << 28) | (high2 << 8) | (index & 0x7FF)
}

fn dst_token(regtype: u8, index: u32, mask: u8) -> u32 {
    reg_token(regtype, index) | ((mask as u32) << 16)
}

fn src_token(regtype: u8, index: u32, swizzle: u8, srcmod: u8) -> u32 {
    reg_token(regtype, index) | ((swizzle as u32) << 16) | ((srcmod as u32) << 24)
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

fn nested_if_ir(nesting: usize) -> ShaderIr {
    let cond = Cond::NonZero {
        src: Src {
            reg: RegRef {
                file: RegFile::Const,
                index: 0,
                relative: None,
            },
            swizzle: Swizzle::identity(),
            modifier: SrcModifier::None,
        },
    };

    let mut inner = Block::new();
    for _ in 0..nesting {
        inner = Block {
            stmts: vec![Stmt::If {
                cond: cond.clone(),
                then_block: inner,
                else_block: None,
            }],
        };
    }

    ShaderIr {
        version: ShaderVersion {
            stage: ShaderStage::Pixel,
            major: 3,
            minor: 0,
        },
        inputs: Vec::new(),
        outputs: Vec::new(),
        samplers: Vec::new(),
        const_defs_f32: Vec::new(),
        const_defs_i32: Vec::new(),
        const_defs_bool: Vec::new(),
        body: inner,
        uses_semantic_locations: false,
    }
}

#[test]
fn ir_snapshot_ps3_tex_ifc() {
    // ps_3_0:
    //   dcl_texcoord0 v0.xy
    //   dcl_2d s0
    //   def c0, 0.5, 0.0, 0.0, 0.0
    //   texld r0, v0, s0
    //   ifc_gt r0.x, c0.x
    //     mov oC0, r0
    //   else
    //     mov oC0, c0
    //   endif
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0.xy
        31u32 | (2u32 << 24) | (5u32 << 16),
        dst_token(1, 0, 0x3),
        // dcl_2d s0
        31u32 | (2u32 << 24) | (2u32 << 16),
        dst_token(10, 0, 0xF),
        // def c0, 0.5, 0, 0, 0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F00_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // texld r0, v0, s0
        opcode_token(0x0042, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // ifc_gt r0.x, c0.x  (compare op 0 = gt)
        opcode_token(41, 2),
        src_token(0, 0, 0x00, 0),
        src_token(2, 0, 0x00, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // else
        opcode_token(42, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        0x0000_FFFF,
    ];

    let token_bytes = to_bytes(&tokens);
    let container = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &token_bytes)]);
    let shdr = dxbc::extract_shader_bytecode(&container).unwrap();
    let decoded = decode_u8_le_bytes(shdr).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    insta::assert_snapshot!(ir.to_string());
}

#[test]
fn ir_builder_rejects_excessive_control_flow_nesting() {
    // Deeply nested `if` blocks can cause recursive IR verification / WGSL generation to blow the
    // Rust stack. Ensure we reject pathological nesting during IR construction.
    let nesting = 256;

    let mut tokens = Vec::new();
    tokens.push(version_token(ShaderStage::Pixel, 3, 0));
    for _ in 0..nesting {
        // if c0
        tokens.push(opcode_token(40, 1));
        tokens.push(src_token(2, 0, 0xE4, 0));
    }
    for _ in 0..nesting {
        // endif
        tokens.push(opcode_token(43, 0));
    }
    tokens.push(0x0000_FFFF);

    let token_bytes = to_bytes(&tokens);
    let decoded = decode_u8_le_bytes(&token_bytes).unwrap();
    let err = build_ir(&decoded).unwrap_err();
    assert!(
        err.message.contains("control flow nesting exceeds maximum"),
        "{err}"
    );
}

#[test]
fn verify_ir_rejects_excessive_control_flow_nesting() {
    let ir = nested_if_ir(256);
    let err = verify_ir(&ir).unwrap_err();
    assert!(
        err.message.contains("control flow nesting exceeds maximum"),
        "{err}"
    );
}

#[test]
fn wgsl_generation_rejects_excessive_control_flow_nesting() {
    let ir = nested_if_ir(256);
    let err = generate_wgsl(&ir).unwrap_err();
    assert!(
        err.message.contains("control flow nesting exceeds maximum"),
        "{err}"
    );
}
