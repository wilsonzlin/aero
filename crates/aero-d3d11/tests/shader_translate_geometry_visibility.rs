use aero_d3d11::shader_translate::reflect_resource_bindings;
use aero_d3d11::{
    BindingKind, DstOperand, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage,
    Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, WriteMask,
};

#[test]
fn geometry_stage_resource_bindings_use_group3_and_compute_visibility() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ConstantBuffer {
            slot: 0,
            reg_count: 1,
        }],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ConstantBuffer { slot: 0, reg: 0 },
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let bindings = reflect_resource_bindings(&module).expect("reflect bindings");
    let cb = bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: 0, .. }))
        .expect("expected cb0 binding");

    assert!(
        cb.visibility.contains(wgpu::ShaderStages::COMPUTE),
        "expected geometry-stage bindings to be visible to the compute stage (GS emulation)"
    );
    assert_eq!(cb.group, 3, "geometry-stage bindings should use @group(3)");
}
