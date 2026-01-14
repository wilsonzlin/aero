use aero_d3d11::shader_translate::reflect_resource_bindings;
use aero_d3d11::{
    BindingKind, DstOperand, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage,
    Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, WriteMask,
};

#[test]
fn extended_stage_resource_bindings_use_group3_and_compute_visibility() {
    fn assert_stage(stage: ShaderStage) {
        let module = Sm4Module {
            stage,
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
            "extended-stage bindings must be visible to the compute emulation path"
        );
        assert_eq!(cb.group, 3, "extended-stage bindings should use @group(3)");
    }

    // WebGPU has no GS/HS/DS stages, so these stages are executed via compute but must keep their
    // own D3D-style binding tables (routed into a dedicated bind group).
    assert_stage(ShaderStage::Geometry);
    assert_stage(ShaderStage::Hull);
    assert_stage(ShaderStage::Domain);
}
