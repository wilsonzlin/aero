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
                    // Use a numeric `f32` literal (`16.0`) for the byte address.
                    //
                    // DXBC register lanes are untyped 32-bit values. Integer-ish operands
                    // (including raw UAV addresses) must consume raw lane bits rather than attempt
                    // float->int heuristics. This should therefore lower to a `u32` constant with
                    // the raw bits `0x41800000` (the IEEE encoding of 16.0), not the numeric
                    // integer `16`.
                    kind: SrcKind::ImmediateF32([16.0f32.to_bits(); 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                uav: UavRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
        .expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // The raw UAV address is a byte offset, so the translator should divide by 4 to index the
    // underlying `array<u32>`. Ensure the `16.0` immediate is treated as raw bits, and ensure no
    // float->int heuristic code appears in the output.
    assert!(
        translated.wgsl.contains("ld_uav_raw_base0"),
        "expected ld_uav_raw base index calculation:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("0x41800000u"),
        "expected float immediate address to be treated as raw bits (0x41800000):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("/ 4u;"),
        "expected byte-to-word address conversion:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("floor("),
        "unexpected float->int heuristic in WGSL:\n{}",
        translated.wgsl
    );

    // Ensure the UAV is declared as a read-write storage buffer and uses the binding model's base.
    assert!(
        translated.wgsl.contains(&format!(
            "@group(2) @binding({}) var<storage, read_write> u0",
            BINDING_BASE_UAV
        )),
        "expected UAV binding decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(translated.wgsl.contains("@compute"));
}
