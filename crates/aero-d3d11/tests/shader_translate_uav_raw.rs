use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, DstOperand, DxbcFile, OperandModifier, RegFile, RegisterRef,
    ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand,
    Swizzle, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

fn build_empty_dxbc() -> Vec<u8> {
    dxbc_test_utils::build_container(&[])
}

fn assert_wgsl_validates(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("generated WGSL failed to validate");
}

#[test]
fn translates_compute_ld_uav_raw_and_validates() {
    let dxbc_bytes = build_empty_dxbc();
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::LdUavRaw {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask(0b0011), // xy
                    saturate: false,
                },
                addr: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                uav: UavRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated =
        translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default()).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure the UAV is declared as a read-write storage buffer and uses the binding model's base.
    assert!(
        translated
            .wgsl
            .contains(&format!(
                "@group(2) @binding({}) var<storage, read_write> u0",
                BINDING_BASE_UAV
            )),
        "expected UAV binding decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(translated.wgsl.contains("@compute"));
}
