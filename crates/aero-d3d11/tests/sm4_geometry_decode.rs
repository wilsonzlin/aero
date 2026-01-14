use std::fs;

use aero_d3d11::{
    GsInputPrimitive, GsOutputTopology, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Decl,
    Sm4Inst, Sm4Program, SrcKind, WriteMask,
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
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1)
            }
        )),
        "expected dcl_inputprimitive in decls: {:#?}",
        module.decls
    );
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(5)
            }
        )),
        "expected dcl_outputtopology in decls: {:#?}",
        module.decls
    );
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::GsMaxOutputVertexCount { max: 4 })),
        "expected dcl_maxvertexcount in decls: {:#?}",
        module.decls
    );

    // The fixture expands each point into a quad (4 emits) and terminates the strip with CUT.
    let emit_count = module
        .instructions
        .iter()
        .filter(|i| matches!(i, Sm4Inst::Emit { stream: 0 }))
        .count();
    assert_eq!(emit_count, 4, "expected four emitted vertices");
    assert!(
        module
            .instructions
            .iter()
            .any(|i| matches!(i, Sm4Inst::Cut { stream: 0 })),
        "expected CUT/RestartStrip instruction"
    );
    assert!(
        matches!(module.instructions.last(), Some(Sm4Inst::Ret)),
        "expected final Ret"
    );

    // Sanity-check the first two moves that pull v0[0] and v1[0] into temps.
    assert!(matches!(
        module.instructions.first(),
        Some(Sm4Inst::Mov { dst, src })
            if dst.reg == RegisterRef { file: RegFile::Temp, index: 0 }
                && dst.mask == WriteMask::XYZW
                && matches!(src.kind, SrcKind::GsInput { reg: 0, vertex: 0 })
    ));
    assert!(module.instructions.iter().any(|i| matches!(
        i,
        Sm4Inst::Mov { dst, src }
            if dst.reg == RegisterRef { file: RegFile::Temp, index: 1 }
                && dst.mask == WriteMask::XYZW
                && matches!(src.kind, SrcKind::GsInput { reg: 1, vertex: 0 })
    )));
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
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(_)
            }
        )),
        "expected dcl_inputprimitive in decls: {:#?}",
        module.decls
    );
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(_)
            }
        )),
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
