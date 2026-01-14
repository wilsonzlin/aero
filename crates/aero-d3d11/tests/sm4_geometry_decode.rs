use std::fs;

use aero_d3d11::{
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Decl, Sm4Inst, Sm4Program,
    SrcKind, SrcOperand, Swizzle, WriteMask,
};

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn decodes_geometry_shader_decls_and_emit_cut() {
    let bytes = load_fixture("gs_cut.dxbc");
    let program = Sm4Program::parse_from_dxbc_bytes(&bytes).expect("SM4 parse");
    assert_eq!(program.stage, ShaderStage::Geometry);
    assert_eq!(program.model, ShaderModel { major: 4, minor: 0 });

    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");
    assert_eq!(module.stage, ShaderStage::Geometry);

    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsInputPrimitive { primitive: 4 })),
        "expected dcl_inputprimitive in decls: {:#?}",
        module.decls
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsOutputTopology { topology: 5 })),
        "expected dcl_outputtopology in decls: {:#?}",
        module.decls
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsMaxOutputVertexCount { max: 3 })),
        "expected dcl_maxvertexcount in decls: {:#?}",
        module.decls
    );

    assert_eq!(module.instructions.len(), 4);
    assert_eq!(
        module.instructions[0],
        Sm4Inst::Mov {
            dst: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            src: SrcOperand {
                kind: SrcKind::GsInput { reg: 0, vertex: 0 },
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }
    );
    assert_eq!(module.instructions[1], Sm4Inst::Emit { stream: 0 });
    assert_eq!(module.instructions[2], Sm4Inst::Cut { stream: 0 });
    assert_eq!(module.instructions[3], Sm4Inst::Ret);
}

#[test]
fn decodes_geometry_shader_emit_stream_and_cut_stream() {
    let bytes = load_fixture("gs_emit_stream_cut_stream.dxbc");
    let program = Sm4Program::parse_from_dxbc_bytes(&bytes).expect("SM4 parse");
    assert_eq!(program.stage, ShaderStage::Geometry);
    assert_eq!(program.model, ShaderModel { major: 5, minor: 0 });

    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");
    assert_eq!(module.stage, ShaderStage::Geometry);

    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsInputPrimitive { primitive: 4 })),
        "expected dcl_inputprimitive in decls: {:#?}",
        module.decls
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsOutputTopology { topology: 5 })),
        "expected dcl_outputtopology in decls: {:#?}",
        module.decls
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsMaxOutputVertexCount { max: 3 })),
        "expected dcl_maxvertexcount in decls: {:#?}",
        module.decls
    );

    assert_eq!(
        module.instructions,
        vec![
            Sm4Inst::Emit { stream: 2 },
            Sm4Inst::Cut { stream: 3 },
            Sm4Inst::Ret,
        ]
    );
}
