use std::fs;

use aero_d3d11::binding_model::{BINDING_BASE_TEXTURE, BINDING_BASE_UAV};
use aero_d3d11::sm4::decode_program;
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BindingKind, BufferKind, BufferRef, DstOperand,
    DxbcFile, OperandModifier, RegFile, RegisterRef, ShaderStage, Sm4Decl, Sm4Inst, Sm4Program,
    SrcKind, SrcOperand, Swizzle, UavRef, WriteMask,
};

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
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
fn decodes_and_translates_compute_uav_store_fixture() {
    let bytes = load_fixture("cs_store_uav_raw.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse failed");
    assert_eq!(program.stage, ShaderStage::Compute);
    assert_eq!(program.model.major, 5);
    assert_eq!(program.model.minor, 0);

    let module = decode_program(&program).expect("SM5 decode failed");

    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
        ],
        "expected fixture to decode into the exact decl list we hand-authored"
    );

    let [store, Sm4Inst::Ret] = module.instructions.as_slice() else {
        panic!(
            "expected fixture to decode into [store_raw, ret] instructions; got: {:?}",
            module.instructions
        );
    };
    let Sm4Inst::StoreRaw {
        uav,
        addr,
        value,
        mask,
    } = store
    else {
        panic!("expected first instruction to be StoreRaw; got: {store:?}");
    };
    assert_eq!(uav.slot, 0);
    assert_eq!(*mask, WriteMask::X);
    assert!(
        matches!(
            addr.kind,
            SrcKind::ImmediateF32(bits) if bits == [0u32; 4]
        ),
        "expected store_raw addr to be immediate 0"
    );
    assert_eq!(addr.swizzle, Swizzle::XYZW);
    assert_eq!(addr.modifier, OperandModifier::None);
    assert!(
        matches!(
            value.kind,
            SrcKind::ImmediateF32(bits) if bits == [0x12345678u32; 4]
        ),
        "expected store_raw value to be immediate 0x12345678"
    );
    assert_eq!(value.swizzle, Swizzle::XYZW);
    assert_eq!(value.modifier, OperandModifier::None);

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    let translated =
        translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translation failed");

    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("@workgroup_size(1, 1, 1)"));
    assert_eq!(translated.stage, ShaderStage::Compute);

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::UavBuffer { slot: 0 }))
        .expect("expected u0 binding in reflection");
    assert_eq!(uav_binding.group, 2);
    assert_eq!(uav_binding.binding, BINDING_BASE_UAV);
    assert_eq!(uav_binding.visibility, wgpu::ShaderStages::COMPUTE);
}

#[test]
fn decodes_and_translates_compute_copy_raw_srv_to_uav_fixture() {
    let bytes = load_fixture("cs_copy_raw_srv_to_uav.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse failed");
    assert_eq!(program.stage, ShaderStage::Compute);
    assert_eq!(program.model.major, 5);
    assert_eq!(program.model.minor, 0);

    let module = decode_program(&program).expect("SM5 decode failed");
    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::ResourceBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
        ],
        "expected fixture to decode into the exact decl list we hand-authored"
    );

    let [ld, store, Sm4Inst::Ret] = module.instructions.as_slice() else {
        panic!(
            "expected fixture to decode into [ld_raw, store_raw, ret] instructions; got: {:?}",
            module.instructions
        );
    };

    assert_eq!(
        *ld,
        Sm4Inst::LdRaw {
            dst: DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            addr: SrcOperand {
                kind: SrcKind::ImmediateF32([0u32; 4]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            buffer: BufferRef { slot: 0 },
        },
        "expected ld_raw r0.xyzw, 0, t0"
    );

    assert_eq!(
        *store,
        Sm4Inst::StoreRaw {
            uav: UavRef { slot: 0 },
            addr: SrcOperand {
                kind: SrcKind::ImmediateF32([0u32; 4]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            value: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            mask: WriteMask::XYZW,
        },
        "expected store_raw u0.xyzw, 0, r0"
    );

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    let translated =
        translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translation failed");

    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("@workgroup_size(1, 1, 1)"));
    assert_eq!(translated.stage, ShaderStage::Compute);

    let srv_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::SrvBuffer { slot: 0 }))
        .expect("expected t0 binding in reflection");
    assert_eq!(srv_binding.group, 2);
    assert_eq!(srv_binding.binding, BINDING_BASE_TEXTURE);
    assert_eq!(srv_binding.visibility, wgpu::ShaderStages::COMPUTE);

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::UavBuffer { slot: 0 }))
        .expect("expected u0 binding in reflection");
    assert_eq!(uav_binding.group, 2);
    assert_eq!(uav_binding.binding, BINDING_BASE_UAV);
    assert_eq!(uav_binding.visibility, wgpu::ShaderStages::COMPUTE);
}

#[test]
fn decodes_and_translates_compute_copy_structured_srv_to_uav_fixture() {
    let bytes = load_fixture("cs_copy_structured_srv_to_uav.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse failed");
    assert_eq!(program.stage, ShaderStage::Compute);
    assert_eq!(program.model.major, 5);
    assert_eq!(program.model.minor, 0);

    let module = decode_program(&program).expect("SM5 decode failed");
    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::ResourceBuffer {
                slot: 0,
                stride: 16,
                kind: BufferKind::Structured,
            },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 16,
                kind: BufferKind::Structured,
            },
        ],
        "expected fixture to decode into the exact decl list we hand-authored"
    );

    let [ld, store, Sm4Inst::Ret] = module.instructions.as_slice() else {
        panic!(
            "expected fixture to decode into [ld_structured, store_structured, ret] instructions; got: {:?}",
            module.instructions
        );
    };

    assert_eq!(
        *ld,
        Sm4Inst::LdStructured {
            dst: DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            index: SrcOperand {
                kind: SrcKind::ImmediateF32([1u32; 4]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            offset: SrcOperand {
                kind: SrcKind::ImmediateF32([0u32; 4]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            buffer: BufferRef { slot: 0 },
        },
        "expected ld_structured r0.xyzw, index=1, offset=0, t0"
    );

    assert_eq!(
        *store,
        Sm4Inst::StoreStructured {
            uav: UavRef { slot: 0 },
            index: SrcOperand {
                kind: SrcKind::ImmediateF32([0u32; 4]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            offset: SrcOperand {
                kind: SrcKind::ImmediateF32([0u32; 4]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            value: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            mask: WriteMask::XYZW,
        },
        "expected store_structured u0.xyzw, index=0, offset=0, r0"
    );

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    let translated =
        translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translation failed");

    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("@workgroup_size(1, 1, 1)"));
    assert_eq!(translated.stage, ShaderStage::Compute);

    let srv_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::SrvBuffer { slot: 0 }))
        .expect("expected t0 binding in reflection");
    assert_eq!(srv_binding.group, 2);
    assert_eq!(srv_binding.binding, BINDING_BASE_TEXTURE);
    assert_eq!(srv_binding.visibility, wgpu::ShaderStages::COMPUTE);

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::UavBuffer { slot: 0 }))
        .expect("expected u0 binding in reflection");
    assert_eq!(uav_binding.group, 2);
    assert_eq!(uav_binding.binding, BINDING_BASE_UAV);
    assert_eq!(uav_binding.visibility, wgpu::ShaderStages::COMPUTE);
}
