use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BufferKind, DxbcFile, FourCC, ShaderModel,
    ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, WriteMask,
};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }
    let total_size = cursor as u32;

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored by parser)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved/unknown
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    for (fourcc, data) in chunks {
        bytes.extend_from_slice(&fourcc.0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }
    assert_eq!(bytes.len(), total_size as usize);
    bytes
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
fn translates_compute_buffer_load_store_raw() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let addr_from_thread_id = SrcOperand {
        kind: SrcKind::Register(aero_d3d11::RegisterRef {
            file: aero_d3d11::RegFile::Input,
            index: 0,
        }),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            // Bind v0 to SV_DispatchThreadID so translation emits global_invocation_id.
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: 20,
            },
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
        instructions: vec![
            Sm4Inst::LdRaw {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                addr: addr_from_thread_id.clone(),
                buffer: aero_d3d11::BufferRef { slot: 0 },
            },
            Sm4Inst::StoreRaw {
                uav: aero_d3d11::UavRef { slot: 0 },
                addr: addr_from_thread_id,
                value: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("@builtin(global_invocation_id)"),
        "expected CsIn to contain global_invocation_id:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("bitcast<f32>(input.global_invocation_id.x)"),
        "expected compute sysvalue lowering to preserve integer bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("t0.data["),
        "expected buffer SRV access in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("u0.data["),
        "expected buffer UAV access in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_compute_buffer_load_store_structured() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let index_from_thread_id = SrcOperand {
        kind: SrcKind::Register(aero_d3d11::RegisterRef {
            file: aero_d3d11::RegFile::Input,
            index: 0,
        }),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };
    let zero = SrcOperand {
        kind: SrcKind::ImmediateF32([0; 4]),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: 20,
            },
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
        instructions: vec![
            Sm4Inst::LdStructured {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                index: index_from_thread_id.clone(),
                offset: zero.clone(),
                buffer: aero_d3d11::BufferRef { slot: 0 },
            },
            Sm4Inst::StoreStructured {
                uav: aero_d3d11::UavRef { slot: 0 },
                index: index_from_thread_id,
                offset: zero,
                value: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("* 16u"),
        "expected structured stride (16 bytes) to appear in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("t0.data["),
        "expected structured SRV access in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("u0.data["),
        "expected structured UAV access in WGSL:\n{}",
        translated.wgsl
    );
}
