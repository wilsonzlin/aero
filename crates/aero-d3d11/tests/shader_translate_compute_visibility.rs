use aero_d3d11::shader_translate::reflect_resource_bindings;
use aero_d3d11::{
    BindingKind, DstOperand, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage,
    Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, WriteMask,
};

#[test]
fn compute_stage_resource_bindings_use_compute_visibility() {
    let module = Sm4Module {
        stage: ShaderStage::Compute,
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

    let bindings = reflect_resource_bindings(&module);
    let cb = bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: 0, .. }))
        .expect("expected cb0 binding");

    assert!(cb.visibility.contains(wgpu::ShaderStages::COMPUTE));
    assert_eq!(cb.group, 2, "compute-stage bindings should use @group(2)");
}

