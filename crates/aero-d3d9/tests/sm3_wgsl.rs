#[cfg(not(target_arch = "wasm32"))]
use std::borrow::Cow;
use std::fs;

use aero_d3d9::dxbc;
use aero_d3d9::sm3::decode::{Opcode, TextureType};
use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{
    build_ir, decode_u32_tokens, decode_u8_le_bytes, generate_wgsl, generate_wgsl_with_options,
    verify_ir, WgslOptions,
};

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/dxbc/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

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

#[cfg(not(target_arch = "wasm32"))]
fn request_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    // `AERO_REQUIRE_WEBGPU=1` means WebGPU is a hard requirement; anything else
    // (including `0`/unset) means tests should skip when no adapter/device is available.
    let require_webgpu = std::env::var("AERO_REQUIRE_WEBGPU")
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);
        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-d3d9-xdg-runtime-{}-sm3-wgsl",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::PRIMARY
        },
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
    }))
    .or_else(|| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
    });
    let adapter = match adapter {
        Some(adapter) => adapter,
        None => {
            if require_webgpu {
                panic!("AERO_REQUIRE_WEBGPU is enabled but wgpu request_adapter returned None");
            }
            eprintln!("skipping WebGPU-dependent test: no suitable adapter");
            return None;
        }
    };

    match pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-d3d9-sm3-wgsl-tests"),
            required_features: wgpu::Features::empty(),
            required_limits: {
                let mut limits = wgpu::Limits::downlevel_defaults();
                // Match `aero-gpu`'s D3D9 executor constants buffer size.
                limits.max_uniform_buffer_binding_size =
                    limits.max_uniform_buffer_binding_size.max(18432);
                limits
            },
        },
        None,
    )) {
        Ok(device) => Some(device),
        Err(err) => {
            if require_webgpu {
                panic!("AERO_REQUIRE_WEBGPU is enabled but request_device failed: {err:?}");
            }
            eprintln!("skipping WebGPU-dependent test: request_device failed: {err:?}");
            None
        }
    }
}

#[test]
fn wgsl_vs20_reads_v0_writes_opos_compiles() {
    // vs_2_0:
    //   mov oPos, v0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),     // oPos
        src_token(1, 0, 0xE4, 0), // v0
        0x0000_FFFF,              // end
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("struct VsInput"), "{wgsl}");
    assert!(wgsl.contains("@location(0) v0"), "{wgsl}");
    assert!(wgsl.contains("@builtin(position)"), "{wgsl}");
}

#[test]
fn wgsl_ps20_reads_t0_and_v0_compiles() {
    // ps_2_0:
    //   add r0, t0, v0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // add r0, t0, v0
        opcode_token(2, 3),
        dst_token(0, 0, 0xF),     // r0
        src_token(3, 0, 0xE4, 0), // t0
        src_token(1, 0, 0xE4, 0), // v0
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(0, 0, 0xE4, 0), // r0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("struct FsIn"), "{wgsl}");
    assert!(wgsl.contains("@location(0) v0"), "{wgsl}");
    // Legacy mapping for t# starts at location 4.
    assert!(wgsl.contains("@location(4) t0"), "{wgsl}");
}

#[test]
fn wgsl_ps30_reads_vpos_compiles() {
    // ps_3_0:
    //   mov r0, vPos
    //   mov oC0, r0
    //   end
    //
    // D3D9 encodes vPos as a MiscType register (regtype 17, index 0).
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov r0, misc0
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),      // r0
        src_token(17, 0, 0xE4, 0), // vPos
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(0, 0, 0xE4, 0), // r0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("@builtin(position)"), "{wgsl}");
    assert!(wgsl.contains("misc0 = input.frag_pos;"), "{wgsl}");
}

#[test]
fn wgsl_ps30_dcl_position_v0_uses_builtin_position() {
    // ps_3_0:
    //   dcl_position v0
    //   mov r0, v0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_position v0
        opcode_token(31, 1),
        dst_token(1, 0, 0xF),
        // mov r0, v0
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(
        wgsl.contains("@builtin(position) frag_pos"),
        "expected POSITION input to map to fragment @builtin(position)\n{wgsl}"
    );
    assert!(
        wgsl.contains("v0 = input.frag_pos;"),
        "expected v0 to be bound from builtin frag_pos\n{wgsl}"
    );
    assert!(
        !wgsl.contains("@location(0) v0"),
        "did not expect POSITION input to become a @location varying\n{wgsl}"
    );
}

#[test]
fn wgsl_ps30_reads_vface_compiles() {
    // ps_3_0:
    //   mov r0, vFace
    //   mov oC0, r0
    //   end
    //
    // D3D9 encodes vFace as a MiscType register (regtype 17, index 1).
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov r0, misc1
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),      // r0
        src_token(17, 1, 0xE4, 0), // vFace
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(0, 0, 0xE4, 0), // r0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("@builtin(front_facing)"), "{wgsl}");
    assert!(
        wgsl.contains("misc1 = vec4<f32>(face, face, face, face);"),
        "{wgsl}"
    );
}

#[test]
fn wgsl_ps30_writes_odepth_compiles() {
    // ps_3_0:
    //   mov oDepth, c0.x
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov oDepth.x, c0.x
        opcode_token(1, 2),
        dst_token(9, 0, 0x1),     // oDepth.x
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("@builtin(frag_depth)"), "{wgsl}");
    assert!(wgsl.contains("out.depth = oDepth.x;"), "{wgsl}");
}

#[test]
fn wgsl_texld_emits_texture_sample() {
    // ps_2_0:
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.wgsl.contains("textureSample("), "{}", wgsl.wgsl);
    assert_eq!(wgsl.bind_group_layout.sampler_group, 2);
    assert_eq!(
        wgsl.bind_group_layout.sampler_bindings.get(&0),
        Some(&(0, 1))
    );
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::Texture2D)
    );
    assert!(
        wgsl.wgsl
            .contains("@group(2) @binding(0) var tex0: texture_2d<f32>;"),
        "{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl
            .contains("@group(2) @binding(1) var samp0: sampler;"),
        "{}",
        wgsl.wgsl
    );
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::Texture2D)
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_cube_sampler_emits_texture_cube_and_xyz_coords() {
    // ps_3_0:
    //   dcl_cube s0
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_cube s0 (usage_raw=3 encodes TextureCube for sampler decls)
        opcode_token(31, 1) | (3u32 << 16),
        dst_token(10, 0, 0xF),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert_eq!(wgsl.bind_group_layout.sampler_group, 2);
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::TextureCube)
    );
    assert!(
        wgsl.wgsl
            .contains("@group(2) @binding(0) var tex0: texture_cube<f32>;"),
        "{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl
            .contains("textureSample(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz)"),
        "{}",
        wgsl.wgsl
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_volume_sampler_emits_texture_3d_and_xyz_coords() {
    // ps_3_0:
    //   dcl_volume s0
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_volume s0 (usage_raw=4 encodes Texture3D for sampler decls)
        opcode_token(31, 1) | (4u32 << 16),
        dst_token(10, 0, 0xF),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert_eq!(wgsl.bind_group_layout.sampler_group, 2);
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::Texture3D)
    );
    assert!(
        wgsl.wgsl
            .contains("@group(2) @binding(0) var tex0: texture_3d<f32>;"),
        "{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl
            .contains("textureSample(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz)"),
        "{}",
        wgsl.wgsl
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_1d_sampler_emits_texture_1d_and_x_coord() {
    // ps_3_0:
    //   dcl_1d s0
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_1d s0 (usage_raw=1 encodes Texture1D for sampler decls)
        opcode_token(31, 1) | (1u32 << 16),
        dst_token(10, 0, 0xF),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert_eq!(wgsl.bind_group_layout.sampler_group, 2);
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::Texture1D)
    );
    assert!(
        wgsl.wgsl
            .contains("@group(2) @binding(0) var tex0: texture_1d<f32>;"),
        "{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl
            .contains("textureSample(tex0, samp0, (constants.c[CONST_BASE + 0u]).x)"),
        "{}",
        wgsl.wgsl
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_1d_sampler_texldp_emits_projective_divide_x() {
    // ps_3_0:
    //   dcl_1d s0
    //   texldp r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_1d s0
        opcode_token(31, 1) | (1u32 << 16),
        dst_token(10, 0, 0xF),
        // texldp r0, c0, s0 (project flag is opcode_token[16])
        opcode_token(66, 3) | (1u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(
        wgsl.contains(
            "textureSample(tex0, samp0, ((constants.c[CONST_BASE + 0u]).x / (constants.c[CONST_BASE + 0u]).w))"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_1d_sampler_texldd_emits_texture_sample_grad_x() {
    // ps_3_0:
    //   dcl_1d s0
    //   texldd r0, c0, c1, c2, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_1d s0
        opcode_token(31, 1) | (1u32 << 16),
        dst_token(10, 0, 0xF),
        // texldd r0, c0, c1, c2, s0
        opcode_token(93, 5),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains(
            "textureSampleGrad(tex0, samp0, (constants.c[CONST_BASE + 0u]).x, (constants.c[CONST_BASE + 1u]).x, (constants.c[CONST_BASE + 2u]).x)"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_cube_sampler_texldp_emits_projective_divide_xyz() {
    // ps_3_0:
    //   dcl_cube s0
    //   texldp r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_cube s0
        opcode_token(31, 1) | (3u32 << 16),
        dst_token(10, 0, 0xF),
        // texldp r0, c0, s0 (project flag is opcode_token[16])
        opcode_token(66, 3) | (1u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(
        wgsl.contains(
            "textureSample(tex0, samp0, ((constants.c[CONST_BASE + 0u]).xyz / (constants.c[CONST_BASE + 0u]).w))"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_volume_sampler_texldp_emits_projective_divide_xyz() {
    // ps_3_0:
    //   dcl_volume s0
    //   texldp r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_volume s0
        opcode_token(31, 1) | (4u32 << 16),
        dst_token(10, 0, 0xF),
        // texldp r0, c0, s0 (project flag is opcode_token[16])
        opcode_token(66, 3) | (1u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(
        wgsl.contains(
            "textureSample(tex0, samp0, ((constants.c[CONST_BASE + 0u]).xyz / (constants.c[CONST_BASE + 0u]).w))"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texldp_emits_projective_divide() {
    // ps_2_0:
    //   texldp r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // texldp r0, c0, s0 (project flag is opcode_token[16])
        opcode_token(66, 3) | (1u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(
        wgsl.contains("((constants.c[CONST_BASE + 0u]).xy / (constants.c[CONST_BASE + 0u]).w)")
            || wgsl.contains(").xy / (constants.c[CONST_BASE + 0u]).w"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texldb_emits_texture_sample_bias() {
    // ps_3_0:
    //   dcl_2d s0
    //   texldb r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_2d s0 (usage_raw=2 encodes Texture2D for sampler decls)
        opcode_token(31, 1) | (2u32 << 16),
        dst_token(10, 0, 0xF),
        // texldb r0, c0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 3) | (2u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleBias("), "{wgsl}");
    assert!(
        wgsl.contains(
            "textureSampleBias(tex0, samp0, (constants.c[CONST_BASE + 0u]).xy, (constants.c[CONST_BASE + 0u]).w)"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_1d_sampler_texldb_emits_texture_sample_grad_x_with_bias() {
    // ps_3_0:
    //   dcl_1d s0
    //   texldb r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_1d s0 (usage_raw=1 encodes Texture1D for sampler decls)
        opcode_token(31, 1) | (1u32 << 16),
        dst_token(10, 0, 0xF),
        // texldb r0, c0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 3) | (2u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        !wgsl.contains("textureSampleBias("),
        "textureSampleBias is not valid for 1D textures in WGSL\n{wgsl}"
    );
    assert!(
        wgsl.contains(
            "textureSampleGrad(tex0, samp0, (constants.c[CONST_BASE + 0u]).x, (dpdx((constants.c[CONST_BASE + 0u]).x) * exp2((constants.c[CONST_BASE + 0u]).w)), (dpdy((constants.c[CONST_BASE + 0u]).x) * exp2((constants.c[CONST_BASE + 0u]).w)))"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_cube_sampler_texldb_emits_texture_sample_bias_xyz() {
    // ps_3_0:
    //   dcl_cube s0
    //   texldb r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_cube s0 (usage_raw=3 encodes TextureCube for sampler decls)
        opcode_token(31, 1) | (3u32 << 16),
        dst_token(10, 0, 0xF),
        // texldb r0, c0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 3) | (2u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleBias("), "{wgsl}");
    assert!(
        wgsl.contains(
            "textureSampleBias(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz, (constants.c[CONST_BASE + 0u]).w)"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_volume_sampler_texldb_emits_texture_sample_bias_xyz() {
    // ps_3_0:
    //   dcl_volume s0
    //   texldb r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_volume s0 (usage_raw=4 encodes Texture3D for sampler decls)
        opcode_token(31, 1) | (4u32 << 16),
        dst_token(10, 0, 0xF),
        // texldb r0, c0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 3) | (2u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleBias("), "{wgsl}");
    assert!(
        wgsl.contains(
            "textureSampleBias(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz, (constants.c[CONST_BASE + 0u]).w)"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texldd_emits_texture_sample_grad() {
    // ps_3_0:
    //   texldd r0, c0, c1, c2, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texldd r0, c0, c1, c2, s0
        opcode_token(93, 5),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::TexLdd));
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleGrad("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_cube_sampler_texldd_emits_texture_sample_grad_xyz() {
    // ps_3_0:
    //   dcl_cube s0
    //   texldd r0, c0, c1, c2, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_cube s0
        opcode_token(31, 1) | (3u32 << 16),
        dst_token(10, 0, 0xF),
        // texldd r0, c0, c1, c2, s0
        opcode_token(93, 5),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::TexLdd));
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains(
            "textureSampleGrad(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz, (constants.c[CONST_BASE + 1u]).xyz, (constants.c[CONST_BASE + 2u]).xyz)"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_volume_sampler_texldd_emits_texture_sample_grad_xyz() {
    // ps_3_0:
    //   dcl_volume s0
    //   texldd r0, c0, c1, c2, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_volume s0
        opcode_token(31, 1) | (4u32 << 16),
        dst_token(10, 0, 0xF),
        // texldd r0, c0, c1, c2, s0
        opcode_token(93, 5),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::TexLdd));
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains(
            "textureSampleGrad(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz, (constants.c[CONST_BASE + 1u]).xyz, (constants.c[CONST_BASE + 2u]).xyz)"
        ),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs_texld_emits_texture_sample_level() {
    // vs_3_0:
    //   texld r0, c0, s0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.wgsl.contains("@vertex"), "{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("textureSampleLevel("), "{}", wgsl.wgsl);
    assert_eq!(wgsl.bind_group_layout.sampler_group, 1);
    assert_eq!(
        wgsl.bind_group_layout.sampler_bindings.get(&0),
        Some(&(0, 1))
    );
    assert!(
        wgsl.wgsl
            .contains("@group(1) @binding(0) var tex0: texture_2d<f32>;"),
        "{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl
            .contains("@group(1) @binding(1) var samp0: sampler;"),
        "{}",
        wgsl.wgsl
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texldl_emits_texture_sample_level_explicit_lod() {
    // ps_3_0:
    //   texldl r0, c0, s1
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texldl r0, c0, s1
        opcode_token(95, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 1, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::TexLdl));
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.wgsl.contains("textureSampleLevel("), "{}", wgsl.wgsl);
    assert!(
        wgsl.wgsl.contains("(constants.c[CONST_BASE + 0u]).w"),
        "{}",
        wgsl.wgsl
    );
    assert_eq!(wgsl.bind_group_layout.sampler_group, 2);
    assert_eq!(
        wgsl.bind_group_layout.sampler_bindings.get(&1),
        Some(&(2, 3))
    );
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&1),
        Some(&TextureType::Texture2D)
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_1d_sampler_texldl_emits_texture_sample_level_x_lod() {
    // ps_3_0:
    //   dcl_1d s0
    //   texldl r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_1d s0
        opcode_token(31, 1) | (1u32 << 16),
        dst_token(10, 0, 0xF),
        // texldl r0, c0, s0
        opcode_token(95, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::Texture1D)
    );
    assert!(
        wgsl.wgsl
            .contains(
                "textureSampleLevel(tex0, samp0, (constants.c[CONST_BASE + 0u]).x, (constants.c[CONST_BASE + 0u]).w)"
            ),
        "{}",
        wgsl.wgsl
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_cube_sampler_texldl_emits_texture_sample_level_xyz_lod() {
    // ps_3_0:
    //   dcl_cube s0
    //   texldl r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_cube s0
        opcode_token(31, 1) | (3u32 << 16),
        dst_token(10, 0, 0xF),
        // texldl r0, c0, s0
        opcode_token(95, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::TextureCube)
    );
    assert!(
        wgsl.wgsl
            .contains(
                "textureSampleLevel(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz, (constants.c[CONST_BASE + 0u]).w)"
            ),
        "{}",
        wgsl.wgsl
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dcl_volume_sampler_texldl_emits_texture_sample_level_xyz_lod() {
    // ps_3_0:
    //   dcl_volume s0
    //   texldl r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_volume s0
        opcode_token(31, 1) | (4u32 << 16),
        dst_token(10, 0, 0xF),
        // texldl r0, c0, s0
        opcode_token(95, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::Texture3D)
    );
    assert!(
        wgsl.wgsl
            .contains(
                "textureSampleLevel(tex0, samp0, (constants.c[CONST_BASE + 0u]).xyz, (constants.c[CONST_BASE + 0u]).w)"
            ),
        "{}",
        wgsl.wgsl
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs_texldd_is_rejected() {
    // vs_3_0:
    //   texldd r0, c0, c1, c2, s0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        // texldd r0, c0, c1, c2, s0
        opcode_token(93, 5),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::TexLdd));
    let ir = build_ir(&decoded).unwrap();
    let err = verify_ir(&ir).unwrap_err();
    assert!(err.message.contains("only valid in pixel shaders"), "{err}");
}

#[test]
fn wgsl_texld_cube_emits_texture_cube_sample() {
    // ps_3_0:
    //   dcl_cube s0 (legacy encoding: decl token + dst token)
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl <decl_token>, s0
        opcode_token(31, 2),
        3u32 << 27,
        dst_token(10, 0, 0xF),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.wgsl.contains("texture_cube"), "{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("textureSample("), "{}", wgsl.wgsl);
    // Constant registers (`c#`) are accessed via the shared uniform constants buffer. We expect
    // cube sampling to use `xyz` coordinates.
    assert!(
        wgsl.wgsl.contains("(constants.c[CONST_BASE + 0u]).xyz"),
        "{}",
        wgsl.wgsl
    );
    assert_eq!(
        wgsl.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::TextureCube)
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_defb_if_compiles() {
    // ps_3_0:
    //   def c0, 1,0,0,1
    //   def c1, 0,1,0,1
    //   defb b0, true
    //   if b0
    //     mov oC0, c0
    //   else
    //     mov oC0, c1
    //   endif
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 1, 0, 0, 1
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x0000_0000,
        0x0000_0000,
        0x3F80_0000,
        // def c1, 0, 1, 0, 1
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x0000_0000,
        0x3F80_0000,
        0x0000_0000,
        0x3F80_0000,
        // defb b0, true
        opcode_token(83, 2),
        dst_token(14, 0, 0xF),
        1,
        // if b0
        opcode_token(40, 1),
        src_token(14, 0, 0x00, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // else
        opcode_token(42, 0),
        // mov oC0, c1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 1, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();
    assert_eq!(ir.const_defs_bool.len(), 1);

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    // `defb` lowering is an implementation detail (it may be lifted to a module-scope `const` or
    // initialized in `fs_main`). Accept either form but ensure the constant value is preserved.
    assert!(
        wgsl.contains("const b0: vec4<bool> = vec4<bool>(true, true, true, true);")
            || wgsl.contains("const b0 = vec4<bool>(true, true, true, true);")
            || wgsl.contains("b0 = vec4<bool>(true, true, true, true);"),
        "{wgsl}"
    );
    assert!(wgsl.contains("if ("));
}

#[test]
fn wgsl_defi_loop_breakc_compiles() {
    // ps_3_0:
    //   defi i0, 1, 0, 0, 0
    //   loop aL, i0
    //     breakc_ne i0.x, i0.y
    //   endloop
    //   mov oC0, c0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // defi i0, 1, 0, 0, 0
        opcode_token(82, 5),
        dst_token(7, 0, 0xF),
        1,
        0,
        0,
        0,
        // loop aL, i0
        opcode_token(27, 2),
        src_token(15, 0, 0xE4, 0), // aL
        src_token(7, 0, 0xE4, 0),  // i0
        // breakc_ne i0.x, i0.y  (compare op 4 = ne)
        opcode_token(45, 2) | (4u32 << 16),
        src_token(7, 0, 0x00, 0), // i0.xxxx
        src_token(7, 0, 0x55, 0), // i0.yyyy
        // endloop
        opcode_token(29, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();
    assert_eq!(ir.const_defs_i32.len(), 1);

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    // Like `defb`, `defi` lowering may be emitted as a module-scope `const` or initialized in
    // `fs_main`. Accept either form but ensure the constant value is preserved.
    assert!(
        wgsl.contains("const i0: vec4<i32> = vec4<i32>(1, 0, 0, 0);")
            || wgsl.contains("const i0 = vec4<i32>(1, 0, 0, 0);")
            || wgsl.contains("i0 = vec4<i32>(1, 0, 0, 0);"),
        "{wgsl}"
    );
    assert!(wgsl.contains("loop {"), "{wgsl}");
    // Safety cap makes the loop structurally bounded in WGSL.
    assert!(wgsl.contains(">= 1024u"), "{wgsl}");
    assert!(wgsl.contains("if (_aero_loop_step == 0)"), "{wgsl}");
}

#[test]
fn wgsl_frc_cmp_compiles() {
    // ps_2_0:
    //   def c0, 1.25, -2.5, 3.0, -4.0
    //   def c1, 0.0, 0.0, 0.0, 1.0
    //   frc r0, c0
    //   cmp r1, c0, r0, c1
    //   mov oC0, r1
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // def c0, 1.25, -2.5, 3.0, -4.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3FA0_0000, // 1.25
        0xC020_0000, // -2.5
        0x4040_0000, // 3.0
        0xC080_0000, // -4.0
        // def c1, 0, 0, 0, 1
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x3F80_0000,
        // frc r0, c0
        opcode_token(0x0013, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // cmp r1, c0, r0, c1
        opcode_token(0x0058, 4),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0), // cond
        src_token(0, 0, 0xE4, 0), // src_ge
        src_token(2, 1, 0xE4, 0), // src_lt
        // mov oC0, r1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Frc));
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Cmp));

    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("fract("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_setp_and_predication_compiles() {
    // ps_3_0:
    //   def c0, 1,1,1,1
    //   def c1, 0,0,0,0
    //   def c2, 0.25, 0.5, 0.75, 1.0
    //   setp_ge p0, c0.x, c1.x
    //   mov (p0) oC0, c2
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 1,1,1,1
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        // def c1, 0,0,0,0
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // def c2, 0.25, 0.5, 0.75, 1.0
        opcode_token(81, 5),
        dst_token(2, 2, 0xF),
        0x3E80_0000,
        0x3F00_0000,
        0x3F40_0000,
        0x3F80_0000,
        // setp_ge p0, c0.x, c1.x  (compare op 2 = ge)
        opcode_token(94, 3) | (2u32 << 16),
        dst_token(19, 0, 0xF),
        src_token(2, 0, 0x00, 0), // c0.xxxx
        src_token(2, 1, 0x00, 0), // c1.xxxx
        // mov (p0) oC0, c2
        opcode_token(1, 3) | 0x1000_0000, // predicated
        dst_token(8, 0, 0xF),
        src_token(2, 2, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("var<private> p0"), "{wgsl}");
    assert!(wgsl.contains("if ("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dp2_compiles_and_uses_xy() {
    // ps_2_0:
    //   def c0, 1, 2, 3, 4
    //   def c1, 0.5, 1, 1.5, 2
    //   dp2_sat_x2 r0, c0.zwxy, -c1.yxwz
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // def c0, 1, 2, 3, 4
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x4000_0000,
        0x4040_0000,
        0x4080_0000,
        // def c1, 0.5, 1, 1.5, 2
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x3F00_0000,
        0x3F80_0000,
        0x3FC0_0000,
        0x4000_0000,
        // dp2_sat_x2 r0, c0.zwxy, -c1.yxwz
        opcode_token(90, 3) | (3u32 << 20), // modbits: saturate + mul2
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0x4E, 0), // c0.zwxy
        src_token(2, 1, 0xB1, 1), // -c1.yxwz
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Dp2));

    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dot("), "{wgsl}");
    assert!(wgsl.contains(".xy"), "{wgsl}");
    assert!(wgsl.contains("clamp("), "{wgsl}");
    assert!(wgsl.contains("* 2.0"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_lrp_dp2add_compiles() {
    // ps_3_0:
    //   def c0, 0.5, 0.25, -0.5, 2.0
    //   def c1, 1.0, 2.0, 3.0, 4.0
    //   def c2, 0.0, 0.0, 0.0, 0.0
    //   lrp r0, c0, c1, c2
    //   dp2add_sat_x2 r1, c0, c1, c2
    //   mov oC0, r1
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 0.5, 0.25, -0.5, 2.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F00_0000,
        0x3E80_0000,
        0xBF00_0000,
        0x4000_0000,
        // def c1, 1, 2, 3, 4
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x3F80_0000,
        0x4000_0000,
        0x4040_0000,
        0x4080_0000,
        // def c2, 0, 0, 0, 0
        opcode_token(81, 5),
        dst_token(2, 2, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // lrp r0, c0, c1, c2
        opcode_token(18, 4),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        // dp2add_sat_x2 r1, c0, c1, c2  (saturate + mul2)
        opcode_token(89, 4) | (3u32 << 20),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        // mov oC0, r1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Lrp));
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::Dp2Add));

    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("mix("), "{wgsl}");
    assert!(wgsl.contains("dot("), "{wgsl}");
    assert!(wgsl.contains(".xy"), "{wgsl}");
    assert!(wgsl.contains(").x"), "{wgsl}");
    assert!(wgsl.contains("clamp("), "{wgsl}");
    assert!(wgsl.contains("* 2.0"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dsx_dsy_derivatives_compile() {
    // ps_3_0:
    //   def c0, 0.25, 0.5, 0.75, 1.0
    //   def c1, 1.0, 1.0, 1.0, 1.0
    //   def c2, 0.0, 0.0, 0.0, 0.0
    //   setp_ge p0, c1.x, c2.x
    //   dsx (p0) r0, c0
    //   dsy_sat_x2 r1, c0
    //   add r2, r0, r1
    //   mov oC0, r2
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 0.25, 0.5, 0.75, 1.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3E80_0000,
        0x3F00_0000,
        0x3F40_0000,
        0x3F80_0000,
        // def c1, 1.0, 1.0, 1.0, 1.0
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        // def c2, 0.0, 0.0, 0.0, 0.0
        opcode_token(81, 5),
        dst_token(2, 2, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // setp_ge p0, c1.x, c2.x  (compare op 2 = ge)
        opcode_token(94, 3) | (2u32 << 16),
        dst_token(19, 0, 0xF),
        src_token(2, 1, 0x00, 0), // c1.xxxx
        src_token(2, 2, 0x00, 0), // c2.xxxx
        // dsx (p0) r0, c0
        opcode_token(86, 3) | 0x1000_0000, // predicated
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // dsy_sat_x2 r1, c0  (saturate + mul2)
        opcode_token(87, 2) | (3u32 << 20),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0),
        // add r2, r0, r1
        opcode_token(2, 3),
        dst_token(0, 2, 0xF),
        src_token(0, 0, 0xE4, 0),
        src_token(0, 1, 0xE4, 0),
        // mov oC0, r2
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 2, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("dpdy("), "{wgsl}");
    assert!(wgsl.contains("clamp("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dsx_dsy_can_feed_texldd_gradients() {
    // ps_3_0:
    //   dsx r1, t0
    //   dsy r2, t0
    //   texldd r0, t0, r1, r2, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dsx r1, t0
        opcode_token(86, 2),
        dst_token(0, 1, 0xF),
        src_token(3, 0, 0xE4, 0), // t0
        // dsy r2, t0
        opcode_token(87, 2),
        dst_token(0, 2, 0xF),
        src_token(3, 0, 0xE4, 0), // t0
        // texldd r0, t0, r1, r2, s0
        opcode_token(93, 5),
        dst_token(0, 0, 0xF),
        src_token(3, 0, 0xE4, 0),  // t0
        src_token(0, 1, 0xE4, 0),  // r1
        src_token(0, 2, 0xE4, 0),  // r2
        src_token(10, 0, 0xE4, 0), // s0
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Dsx));
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Dsy));
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::TexLdd));

    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("dpdy("), "{wgsl}");
    assert!(wgsl.contains("textureSampleGrad("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_derivative_avoids_non_uniform_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   setp_gt p0, v0.x, c0.x
    //   dsx (p0) r0, v0
    //   mov oC0, r0
    //   end
    //
    // WGSL derivative ops (`dpdx`/`dpdy`) must appear in uniform control flow. A naive predication
    // lowering of `dsx (p0)` as `if (p0) { r0 = dpdx(v0); }` is rejected by naga when `p0` depends
    // on a varying input (here, `v0`).
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // dsx (p0) r0, v0
        opcode_token(86, 3) | 0x1000_0000, // predicated
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),  // v0
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if (p0.x)"),
        "predicated derivatives should not lower to an if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_texld_avoids_non_uniform_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   setp_gt p0, v0.x, c0.x
    //   texld (p0) r0, v0, s0
    //   mov oC0, r0
    //   end
    //
    // In WGSL/WebGPU, `textureSample()` uses implicit derivatives and must be executed in uniform
    // control flow. A naive predication lowering of `texld (p0)` as `if (p0) { r0 = textureSample(...) }`
    // is rejected by naga when `p0` depends on a varying input (here, `v0`).
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // texld (p0) r0, v0, s0
        opcode_token(66, 4) | 0x1000_0000, // predicated
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),  // v0
        src_token(10, 0, 0xE4, 0), // s0
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if (p0.x)"),
        "predicated texld should not lower to an if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_texldb_avoids_non_uniform_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   setp_gt p0, v0.x, c0.x
    //   texldb (p0) r0, v0, s0
    //   mov oC0, r0
    //   end
    //
    // Like `texld`, `texldb` uses implicit derivatives and must be executed in uniform control flow.
    // Our predication lowering should use `select` rather than `if (p0.x) { ... }`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // texldb (p0) r0, v0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 4) | 0x1000_0000 | (2u32 << 16), // predicated
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),  // v0
        src_token(10, 0, 0xE4, 0), // s0
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleBias("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if (p0.x)"),
        "predicated texldb should not lower to an if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_texldb_1d_avoids_non_uniform_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   dcl_1d s0
    //   setp_gt p0, v0.x, c0.x
    //   texldb (p0) r0, v0, s0
    //   mov oC0, r0
    //   end
    //
    // For 1D textures, WGSL does not support `textureSampleBias`. Our lowering uses `dpdx`/`dpdy`
    // scaled by `exp2(bias)` and calls `textureSampleGrad`, which must still appear in uniform
    // control flow when predicated.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_1d s0
        opcode_token(31, 1) | (1u32 << 16),
        dst_token(10, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // texldb (p0) r0, v0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 4) | 0x1000_0000 | (2u32 << 16), // predicated
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),  // v0
        src_token(10, 0, 0xE4, 0), // s0
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains("textureSampleGrad("),
        "expected texldb(1D) to lower via textureSampleGrad; got:\n{wgsl}"
    );
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("dpdy("), "{wgsl}");
    assert!(wgsl.contains("exp2("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if (p0.x)"),
        "predicated texldb should not lower to an if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_texldp_avoids_non_uniform_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   setp_gt p0, v0.x, c0.x
    //   texldp (p0) r0, v0, s0
    //   mov oC0, r0
    //   end
    //
    // Like `texld`, `texldp` uses implicit derivatives and must be executed in uniform control
    // flow. Our predication lowering should use `select` rather than `if (p0.x) { ... }`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // texldp (p0) r0, v0, s0 (project flag is opcode_token[16])
        opcode_token(66, 4) | 0x1000_0000 | (1u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),  // v0
        src_token(10, 0, 0xE4, 0), // s0
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        wgsl.contains("/ (v0).w") || wgsl.contains(").w)"),
        "expected projective divide in texldp; got:\n{wgsl}"
    );
    assert!(
        !wgsl.contains("if (p0.x)"),
        "predicated texldp should not lower to an if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     texld r0, v0, s0
    //   endif
    //   mov oC0, r0
    //   end
    //
    // In WGSL/WebGPU, `textureSample()` uses implicit derivatives and must be executed in uniform
    // control flow. This test ensures we don't emit it behind a potentially non-uniform SM3 `if`
    // when the `if` guards a single `texld`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // texld r0, v0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "non-uniform if guarding a single texld should lower to select; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_else_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   dcl_texcoord1 v1
    //   if v0.x
    //     texld r0, v0, s0
    //   else
    //     texld r0, v1, s0
    //   endif
    //   mov oC0, r0
    //   end
    //
    // In WGSL/WebGPU, `textureSample()` uses implicit derivatives and must be executed in uniform
    // control flow. This test ensures we don't emit it behind a potentially non-uniform SM3
    // if/else when both branches are a single `texld`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_texcoord1 v1  (usage_index 1 in opcode_token[20..24])
        opcode_token(31, 1) | (5u32 << 16) | (1u32 << 20),
        dst_token(1, 1, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // texld r0, v0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // else
        opcode_token(42, 0),
        // texld r0, v1, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 1, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert_eq!(wgsl.matches("textureSample(").count(), 2, "{wgsl}");
    assert!(wgsl.matches("select(").count() >= 2, "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "non-uniform if/else guarding only texld should lower to select; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_texldb_1d_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   dcl_1d s0
    //   if v0.x
    //     texldb r0, v0, s0
    //   endif
    //   mov oC0, r0
    //   end
    //
    // For 1D textures, texldb uses our dpdx/dpdy+exp2(bias)+textureSampleGrad lowering, which also
    // must appear in uniform control flow. This test ensures we still avoid emitting an `if`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_1d s0
        opcode_token(31, 1) | (1u32 << 16),
        dst_token(10, 0, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // texldb r0, v0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 3) | (2u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleGrad("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "non-uniform if guarding a single texldb(1D) should lower to select; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_texldb_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     texldb r0, v0, s0
    //   endif
    //   mov oC0, r0
    //   end
    //
    // Like `texld`, `texldb` uses implicit derivatives and must appear in uniform control flow in
    // WGSL. This test ensures we lower `if (cond) { texldb }` to a branchless `select(...)`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // texldb r0, v0, s0 (specific field is opcode_token[16..19], where 2 = texldb)
        opcode_token(66, 3) | (2u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleBias("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "non-uniform if guarding a single texldb should lower to select; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_predicated_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   setp_gt p0, v0.x, c0.x
    //   if v0.y
    //     texld (p0) r0, v0, s0
    //   endif
    //   mov oC0, r0
    //   end
    //
    // Ensure we also avoid non-uniform control flow when the single op guarded by the `if` is
    // itself predicated.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // if v0.y
        opcode_token(40, 1),
        src_token(1, 0, 0x55, 0), // v0.yyyy
        // texld (p0) r0, v0, s0
        opcode_token(66, 4) | 0x1000_0000,
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "non-uniform if guarding a predicated texld should lower to select; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_else_predicated_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   dcl_texcoord1 v1
    //   setp_gt p0, v0.x, c0.x
    //   if v0.y
    //     texld (p0) r0, v0, s0
    //   else
    //     texld (p0) r0, v1, s0
    //   endif
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_texcoord1 v1  (usage_index 1 in opcode_token[20..24])
        opcode_token(31, 1) | (5u32 << 16) | (1u32 << 20),
        dst_token(1, 1, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // if v0.y
        opcode_token(40, 1),
        src_token(1, 0, 0x55, 0), // v0.yyyy
        // texld (p0) r0, v0, s0
        opcode_token(66, 4) | 0x1000_0000,
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // else
        opcode_token(42, 0),
        // texld (p0) r0, v1, s0
        opcode_token(66, 4) | 0x1000_0000,
        dst_token(0, 0, 0xF),
        src_token(1, 1, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert_eq!(wgsl.matches("textureSample(").count(), 2, "{wgsl}");
    assert!(wgsl.matches("select(").count() >= 2, "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "non-uniform if/else guarding predicated texld should lower to select; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_mov_then_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     mov r1, v0
    //     texld r0, r1, s0
    //   endif
    //   mov oC0, r0
    //   end
    //
    // Ensure the if-hoisting logic can handle the common pattern where the coordinate is first
    // copied to a temp register.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // mov r1, v0
        opcode_token(1, 2),
        dst_token(0, 1, 0xF),
        src_token(1, 0, 0xE4, 0),
        // texld r0, r1, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "expected mov+texld if to lower branchlessly; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_mov_then_dsx_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     mov r1, v0
    //     dsx r0, r1
    //   endif
    //   mov oC0, r0
    //   end
    //
    // Like `textureSample`, `dpdx` must appear in uniform control flow. Ensure we can hoist `dsx`
    // even when it depends on a temp register written earlier in the branch.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // mov r1, v0
        opcode_token(1, 2),
        dst_token(0, 1, 0xF),
        src_token(1, 0, 0xE4, 0),
        // dsx r0, r1
        opcode_token(86, 2),
        dst_token(0, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "expected mov+dsx if to lower branchlessly; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_add_then_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     add r1, v0, c0
    //     texld r0, r1, s0
    //   endif
    //   mov oC0, r0
    //   end
    //
    // Ensure we can hoist implicit-derivative sampling even when it is not the first statement in
    // the branch.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0.x
        opcode_token(40, 1),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        // add r1, v0, c0
        opcode_token(2, 3),
        dst_token(0, 1, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(2, 0, 0xE4, 0),
        // texld r0, r1, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_nested_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     if v0.y
    //       texld r0, v0, s0
    //     endif
    //   endif
    //   mov oC0, r0
    //   end
    //
    // A nested `if` can still put the hoisted `textureSample` under the outer non-uniform branch if
    // we only rewrite the inner `if`. Ensure we predicate through nested `if` trees.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0.x
        opcode_token(40, 1),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        // if v0.y
        opcode_token(40, 1),
        src_token(1, 0, 0x55, 0), // v0.yyyy
        // texld r0, v0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif (inner)
        opcode_token(43, 0),
        // endif (outer)
        opcode_token(43, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_two_texld_avoids_invalid_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     texld r0, v0, s0
    //     texld r1, v0, s0
    //   endif
    //   mov oC0, r1
    //   end
    //
    // `textureSample()` must be executed in uniform control flow. Ensure we hoist multiple
    // consecutive `texld` ops out of a potentially non-uniform `if`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // texld r0, v0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // texld r1, v0, s0
        opcode_token(66, 3),
        dst_token(0, 1, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert_eq!(wgsl.matches("textureSample(").count(), 2, "{wgsl}");
    assert!(wgsl.matches("select(").count() >= 2, "{wgsl}");
    assert!(
        !wgsl.contains("if ("),
        "non-uniform if guarding only texld should lower to select; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_else_texld_twice_followed_by_mov_hoists_texture_sample() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   dcl_texcoord1 v1
    //   if v0.x
    //     mov r2, v0
    //   else
    //     texld r0, v1, s0
    //     texld r1, v1, s0
    //     mov r2, r1
    //   endif
    //   mov oC0, r2
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_texcoord1 v1  (usage_index 1 in opcode_token[20..24])
        opcode_token(31, 1) | (5u32 << 16) | (1u32 << 20),
        dst_token(1, 1, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // mov r2, v0
        opcode_token(1, 2),
        dst_token(0, 2, 0xF),
        src_token(1, 0, 0xE4, 0),
        // else
        opcode_token(42, 0),
        // texld r0, v1, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 1, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // texld r1, v1, s0
        opcode_token(66, 3),
        dst_token(0, 1, 0xF),
        src_token(1, 1, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov r2, r1
        opcode_token(1, 2),
        dst_token(0, 2, 0xF),
        src_token(0, 1, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r2
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 2, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert_eq!(wgsl.matches("textureSample(").count(), 2, "{wgsl}");
    assert!(wgsl.matches("select(").count() >= 2, "{wgsl}");

    let if_pos = wgsl.find("if (").unwrap();
    for (pos, _) in wgsl.match_indices("textureSample(") {
        assert!(
            pos < if_pos,
            "expected textureSample to be hoisted out of the if; got:\n{wgsl}"
        );
    }

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_texld_followed_by_mov_hoists_texture_sample() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   if v0.x
    //     texld r0, v0, s0
    //     mov r1, r0
    //   endif
    //   mov oC0, r1
    //   end
    //
    // Ensure we hoist implicit-derivative texture sampling even when the `if` body contains more
    // statements than a single `texld`.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // texld r0, v0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov r1, r0
        opcode_token(1, 2),
        dst_token(0, 1, 0xF),
        src_token(0, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    let sample_pos = wgsl.find("textureSample(").unwrap();
    let if_pos = wgsl.find("if (").unwrap();
    assert!(
        sample_pos < if_pos,
        "expected textureSample to be hoisted out of the if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_nonuniform_if_else_texld_followed_by_mov_hoists_texture_sample() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   dcl_texcoord1 v1
    //   if v0.x
    //     mov r1, v0
    //   else
    //     texld r0, v1, s0
    //     mov r1, r0
    //   endif
    //   mov oC0, r1
    //   end
    //
    // Ensure hoisting also works when the sensitive op is in the `else` branch and the branch
    // contains additional statements.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_texcoord1 v1  (usage_index 1 in opcode_token[20..24])
        opcode_token(31, 1) | (5u32 << 16) | (1u32 << 20),
        dst_token(1, 1, 0xF),
        // if v0
        opcode_token(40, 1),
        src_token(1, 0, 0xE4, 0),
        // mov r1, v0
        opcode_token(1, 2),
        dst_token(0, 1, 0xF),
        src_token(1, 0, 0xE4, 0),
        // else
        opcode_token(42, 0),
        // texld r0, v1, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 1, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov r1, r0
        opcode_token(1, 2),
        dst_token(0, 1, 0xF),
        src_token(0, 0, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // mov oC0, r1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    let sample_pos = wgsl.find("textureSample(").unwrap();
    let if_pos = wgsl.find("if (").unwrap();
    assert!(
        sample_pos < if_pos,
        "expected textureSample to be hoisted out of the if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_texldd_is_valid_with_non_uniform_predicate() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   setp_gt p0, v0.x, c0.x
    //   texldd (p0) r0, v0, c1, c2, s0
    //   mov oC0, r0
    //   end
    //
    // `texldd` maps to WGSL `textureSampleGrad`, which uses explicit gradients. Unlike `texld`
    // (`textureSample`), this should validate even when predicated on a non-uniform condition.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(94, 3),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // texldd (p0) r0, v0, c1, c2, s0
        opcode_token(93, 6) | 0x1000_0000, // predicated
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),  // v0
        src_token(2, 1, 0xE4, 0),  // c1 (ddx)
        src_token(2, 2, 0xE4, 0),  // c2 (ddy)
        src_token(10, 0, 0xE4, 0), // s0
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleGrad("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texkill_is_conditional() {
    // ps_3_0:
    //   texkill r0
    //   mov oC0, c0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texkill r0
        opcode_token(65, 1),
        src_token(0, 0, 0xE4, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    // Ensure `texkill` generates the D3D9 rule (discard if any component < 0), not an unconditional
    // discard.
    assert!(wgsl.contains("if (any("), "{wgsl}");
    assert!(wgsl.contains("< vec4<f32>(0.0)"), "{wgsl}");
    assert!(wgsl.contains("discard;"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_texkill_is_nested_under_if() {
    // ps_3_0:
    //   texkill (p0) r0
    //   mov oC0, c0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texkill (p0) r0
        opcode_token(65, 2) | 0x1000_0000, // predicated
        src_token(0, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("if (p0.x)"), "{wgsl}");
    let pred_if = wgsl.find("if (p0.x)").expect("predicate if");
    assert!(wgsl[pred_if..].contains("if (any("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs_outputs_and_ps_inputs_use_consistent_locations() {
    // vs_2_0:
    //   dcl_positiont v0
    //   dcl_color0 v7
    //   mov oPos, v0
    //   mov oD0, v7
    //   end
    //
    // ps_2_0:
    //   mov oC0, v0
    //   end
    //
    // The vertex shader should expose oD0 at @location(0), and the pixel shader should read v0
    // from @location(0). The VS also remaps COLOR0 v7 -> @location(6) via StandardLocationMap.
    let vs_tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_positiont v0
        31u32 | (2u32 << 24) | (9u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_color0 v7
        31u32 | (2u32 << 24) | (10u32 << 16),
        dst_token(1, 7, 0xF),
        // mov oPos, v0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // mov oD0, v7
        opcode_token(1, 2),
        dst_token(5, 0, 0xF),
        src_token(1, 7, 0xE4, 0),
        0x0000_FFFF,
    ];
    let ps_tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // mov oC0, v0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let vs_decoded = decode_u32_tokens(&vs_tokens).unwrap();
    let vs_ir = build_ir(&vs_decoded).unwrap();
    verify_ir(&vs_ir).unwrap();
    let vs_wgsl = generate_wgsl(&vs_ir).unwrap().wgsl;
    assert!(vs_wgsl.contains("@location(6) v6"), "{vs_wgsl}");
    assert!(vs_wgsl.contains("@location(0) oD0"), "{vs_wgsl}");

    let ps_decoded = decode_u32_tokens(&ps_tokens).unwrap();
    let ps_ir = build_ir(&ps_decoded).unwrap();
    verify_ir(&ps_ir).unwrap();
    let ps_wgsl = generate_wgsl(&ps_ir).unwrap().wgsl;
    assert!(ps_wgsl.contains("struct FsIn"), "{ps_wgsl}");
    assert!(ps_wgsl.contains("@location(0) v0"), "{ps_wgsl}");

    // Ensure both shaders are valid WGSL modules.
    let vs_mod = naga::front::wgsl::parse_str(&vs_wgsl).expect("vs wgsl parse");
    let ps_mod = naga::front::wgsl::parse_str(&ps_wgsl).expect("ps wgsl parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator.validate(&vs_mod).expect("vs wgsl validate");
    validator.validate(&ps_mod).expect("ps wgsl validate");
}

#[test]
fn wgsl_missing_dcl_uses_v0_writes_oc0_compiles() {
    // ps_2_0:
    //   mov oC0, v0
    //   end
    //
    // Some real-world SM2 shaders omit `dcl` declarations entirely. The WGSL backend must still
    // declare input/output interface variables based on register usage.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // mov oC0, v0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(1, 0, 0xE4, 0), // v0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn sm3_translate_to_wgsl_wrapper_produces_bind_layout() {
    // ps_2_0:
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }

    let out = aero_d3d9::sm3::wgsl::translate_to_wgsl(&bytes).unwrap();
    assert_eq!(out.version.stage, ShaderStage::Pixel);
    assert_eq!(out.entry_point, "fs_main");
    assert_eq!(out.bind_group_layout.sampler_group, 2);
    assert_eq!(
        out.bind_group_layout.sampler_bindings.get(&0),
        Some(&(0, 1))
    );
    assert_eq!(
        out.bind_group_layout.sampler_texture_types.get(&0),
        Some(&TextureType::Texture2D)
    );

    let module = naga::front::wgsl::parse_str(&out.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn sm3_translate_to_wgsl_with_options_emits_half_pixel_uniform() {
    // vs_2_0:
    //   mov oPos, v0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),     // oPos
        src_token(1, 0, 0xE4, 0), // v0
        0x0000_FFFF,              // end
    ];

    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }

    let out = aero_d3d9::sm3::wgsl::translate_to_wgsl_with_options(
        &bytes,
        aero_d3d9::sm3::wgsl::WgslOptions {
            half_pixel_center: true,
        },
    )
    .unwrap();
    assert_eq!(out.version.stage, ShaderStage::Vertex);
    assert_eq!(out.entry_point, "vs_main");
    assert_eq!(out.bind_group_layout.sampler_group, 1);
    assert!(
        out.wgsl.contains("@group(3) @binding(0) var<uniform> half_pixel: HalfPixel;"),
        "{}",
        out.wgsl
    );
    assert!(
        out.wgsl.contains("out.pos.x = out.pos.x - half_pixel.inv_viewport.x * out.pos.w;"),
        "{}",
        out.wgsl
    );
    assert!(
        out.wgsl.contains("out.pos.y = out.pos.y + half_pixel.inv_viewport.y * out.pos.w;"),
        "{}",
        out.wgsl
    );

    let module = naga::front::wgsl::parse_str(&out.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_ps3_vpos_misctype_builtin_compiles() {
    // ps_3_0:
    //   mov oC0, misc0  (vPos)
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov oC0, misc0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(17, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains("@builtin(position) frag_pos: vec4<f32>"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs2_m4x4_fixture_compiles() {
    let bytes = load_fixture("vs_2_0_simple.dxbc");
    let shdr = dxbc::extract_shader_bytecode(&bytes).expect("extract shader bytecode");

    let decoded = decode_u8_le_bytes(shdr).expect("decode");
    let ir = build_ir(&decoded).expect("build ir");
    verify_ir(&ir).expect("verify");
    let wgsl = generate_wgsl(&ir).expect("wgsl").wgsl;

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("@vertex"));
    assert!(wgsl.contains("dot("));
}

#[test]
fn wgsl_dp2add_compiles() {
    // vs_2_0:
    //   dcl_position v0
    //   dp2add r0, v0, c0, c1
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_position v0
        opcode_token(31, 1),
        dst_token(1, 0, 0xF),
        // dp2add r0, v0, c0, c1
        opcode_token(89, 4),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dot("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_abs_compiles() {
    // vs_2_0:
    //   dcl_position v0
    //   abs r0, v0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_position v0
        opcode_token(31, 1),
        dst_token(1, 0, 0xF),
        // abs r0, v0
        opcode_token(35, 2),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("abs("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_sgn_compiles() {
    // vs_2_0:
    //   dcl_position v0
    //   sgn r0, v0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_position v0
        opcode_token(31, 1),
        dst_token(1, 0, 0xF),
        // sgn r0, v0
        opcode_token(34, 2),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("sign("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_crs_compiles() {
    // vs_2_0:
    //   dcl_position v0
    //   crs r0, v0, v0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_position v0
        opcode_token(31, 1),
        dst_token(1, 0, 0xF),
        // crs r0, v0, v0
        opcode_token(33, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(1, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("cross("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dst_compiles() {
    // vs_2_0:
    //   dcl_position v0
    //   dst r0, v0, v0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_position v0
        opcode_token(31, 1),
        dst_token(1, 0, 0xF),
        // dst r0, v0, v0
        opcode_token(17, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(1, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    // Ensure we exercised the custom lowering (`dst` has fixed/packed component semantics).
    assert!(wgsl.contains("vec4<f32>(1.0,"), "{wgsl}");
    assert!(wgsl.contains(".y *"), "{wgsl}");
    assert!(wgsl.contains(").z,"), "{wgsl}");
    assert!(wgsl.contains(").w)"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_ret_ends_shader_compiles() {
    // vs_3_0:
    //   dcl_position v0
    //   mov oPos, v0
    //   ret
    //   end
    //
    // Some real-world SM3 shaders use `ret` to exit the main program (subroutines unsupported).
    let tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        // dcl_position v0
        opcode_token(31, 1),
        dst_token(1, 0, 0xF),
        // mov oPos, v0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // ret
        opcode_token(28, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("@vertex"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_ps3_vface_misctype_builtin_compiles() {
    // ps_3_0:
    //   mov oC0, misc1  (vFace)
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov oC0, misc1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(17, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains("@builtin(front_facing) front_facing: bool"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn sm3_wgsl_is_compatible_with_aerogpu_d3d9_pipeline_layout() {
    // Coarse integration test that SM3 WGSL bindings match the existing AeroGPU D3D9 executor
    // pipeline layout contract:
    // - group(0): constants (VERTEX+FRAGMENT)
    //   - binding(0): float4 constants (`c#`)
    //   - binding(1): int4 constants (`i#`)
    //   - binding(2): bool constants (`b#`, stored as `vec4<u32>` per register)
    // - group(1): VS samplers (VERTEX only)
    // - group(2): PS samplers (FRAGMENT only)
    // - group(3): optional half-pixel-center adjustment uniform (VERTEX only)
    //
    // This catches regressions where SM3 WGSL sampler declarations drift back to group(0) or use
    // incorrect binding numbering.
    let Some((device, queue)) = request_device() else {
        return;
    };

    // vs_3_0:
    //   texld r0, c0, s0
    //   texld r1, c0, s1
    //   add r0, r0, r1
    //   mov oPos, r0
    //   end
    let vs_tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        opcode_token(66, 3),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 1, 0xE4, 0),
        opcode_token(2, 3),
        dst_token(0, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        src_token(0, 1, 0xE4, 0),
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    // ps_2_0:
    //   texld r0, c0, s0
    //   texld r1, c0, s1
    //   add r0, r0, r1
    //   mov oC0, r0
    //   end
    let ps_tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        opcode_token(66, 3),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 1, 0xE4, 0),
        opcode_token(2, 3),
        dst_token(0, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        src_token(0, 1, 0xE4, 0),
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let vs_decoded = decode_u32_tokens(&vs_tokens).unwrap();
    let vs_ir = build_ir(&vs_decoded).unwrap();
    verify_ir(&vs_ir).unwrap();

    let ps_decoded = decode_u32_tokens(&ps_tokens).unwrap();
    let ps_ir = build_ir(&ps_decoded).unwrap();
    verify_ir(&ps_ir).unwrap();
    let ps_out = generate_wgsl(&ps_ir).unwrap();
    let ps_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sm3-wgsl-test.ps"),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(ps_out.wgsl.clone())),
    });

    // Must match the executor's constants bind group layout: a single constants uniform buffer
    // containing float + int + bool banks for VS+PS.
    //
    // Constants buffer layout:
    // - float bank: 512 * vec4<f32>
    // - int bank:   512 * vec4<i32>
    // - bool bank:  128 * vec4<u32> (4 bools per vec4 lane)
    const CONSTANTS_BUFFER_SIZE_BYTES: u64 = 18432;

    let constants_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sm3-wgsl-test.constants_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(CONSTANTS_BUFFER_SIZE_BYTES),
            },
            count: None,
        }],
    });
    let samplers_vs_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sm3-wgsl-test.samplers_vs_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let samplers_ps_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sm3-wgsl-test.samplers_ps_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let half_pixel_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sm3-wgsl-test.half_pixel_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(16),
            },
            count: None,
        }],
    });
    let half_pixel_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sm3-wgsl-test.half_pixel"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&half_pixel_buffer, 0, &[0u8; 16]);
    let half_pixel_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sm3-wgsl-test.half_pixel_bg"),
        layout: &half_pixel_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: half_pixel_buffer.as_entire_binding(),
        }],
    });

    // Also run a tiny draw that binds all expected bind groups. This catches cases where wgpu
    // defers certain binding/layout validation until draw/submit.
    let constants_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sm3-wgsl-test.constants"),
        size: CONSTANTS_BUFFER_SIZE_BYTES,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // Keep shader execution deterministic (avoid uninitialized uniform buffer contents during the
    // draw below).
    let constants_init = vec![0u8; CONSTANTS_BUFFER_SIZE_BYTES as usize];
    queue.write_buffer(&constants_buffer, 0, &constants_init);
    let constants_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sm3-wgsl-test.constants_bg"),
        layout: &constants_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: constants_buffer.as_entire_binding(),
        }],
    });

    // 1x1 RGBA8 texture containing vec4(0,0,0,1) so the vertex shader writes a valid position.
    let sample_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sm3-wgsl-test.sample_tex"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let sample_tex_view = sample_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let sample_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("sm3-wgsl-test.sample_sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    // wgpu requires bytes_per_row alignment even for queue.write_texture, so pad to 256.
    let mut tex_row = vec![0u8; wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize];
    tex_row[..4].copy_from_slice(&[0, 0, 0, 255]);
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &sample_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &tex_row,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );

    let samplers_vs_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sm3-wgsl-test.samplers_vs_bg"),
        layout: &samplers_vs_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&sample_tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sample_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&sample_tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sample_sampler),
            },
        ],
    });
    let samplers_ps_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sm3-wgsl-test.samplers_ps_bg"),
        layout: &samplers_ps_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&sample_tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sample_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&sample_tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sample_sampler),
            },
        ],
    });

    let render_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sm3-wgsl-test.render_tex"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let render_view = render_tex.create_view(&wgpu::TextureViewDescriptor::default());

    for half_pixel_center in [false, true] {
        // Re-generate the VS WGSL per-case so we exercise both the default path and
        // `WgslOptions::half_pixel_center` in the context of the executor's pipeline layout.
        let vs_out = generate_wgsl_with_options(&vs_ir, WgslOptions { half_pixel_center }).unwrap();

        let pipeline_layout = if half_pixel_center {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sm3-wgsl-test.pipeline_layout"),
                bind_group_layouts: &[
                    &constants_bgl,
                    &samplers_vs_bgl,
                    &samplers_ps_bgl,
                    &half_pixel_bgl,
                ],
                push_constant_ranges: &[],
            })
        } else {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sm3-wgsl-test.pipeline_layout"),
                bind_group_layouts: &[&constants_bgl, &samplers_vs_bgl, &samplers_ps_bgl],
                push_constant_ranges: &[],
            })
        };

        let vs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sm3-wgsl-test.vs"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(vs_out.wgsl.clone())),
        });

        // Pipeline creation must validate shader bind groups against the provided pipeline layout.
        //
        // Note: wgpu may return a dummy pipeline object and report validation errors via the
        // device's error callback, so use an error scope to make failures visible to the test
        // harness.
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sm3-wgsl-test.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &vs_module,
                entry_point: vs_out.entry_point,
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &ps_module,
                entry_point: ps_out.entry_point,
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        device.poll(wgpu::Maintain::Poll);
        let err = pollster::block_on(device.pop_error_scope());
        assert!(
            err.is_none(),
            "wgpu validation error (pipeline, half_pixel_center={half_pixel_center}): {err:?}"
        );

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sm3-wgsl-test.encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sm3-wgsl-test.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &render_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &constants_bg, &[]);
            pass.set_bind_group(1, &samplers_vs_bg, &[]);
            pass.set_bind_group(2, &samplers_ps_bg, &[]);
            if half_pixel_center {
                pass.set_bind_group(3, &half_pixel_bg, &[]);
            }
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        device.poll(wgpu::Maintain::Poll);
        let err = pollster::block_on(device.pop_error_scope());
        assert!(
            err.is_none(),
            "wgpu validation error (draw, half_pixel_center={half_pixel_center}): {err:?}"
        );
    }
}

#[test]
fn wgsl_vs_half_pixel_center_enabled_emits_uniform_and_adjustment() {
    // vs_2_0:
    //   mov oPos, v0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),     // oPos
        src_token(1, 0, 0xE4, 0), // v0
        0x0000_FFFF,              // end
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl_with_options(
        &ir,
        WgslOptions {
            half_pixel_center: true,
        },
    )
    .unwrap()
    .wgsl;

    assert!(
        wgsl.contains("struct HalfPixel { inv_viewport: vec2<f32>, _pad: vec2<f32>, };"),
        "{wgsl}"
    );
    assert!(
        wgsl.contains("@group(3) @binding(0) var<uniform> half_pixel: HalfPixel;"),
        "{wgsl}"
    );
    assert!(
        wgsl.contains("out.pos.x = out.pos.x - half_pixel.inv_viewport.x * out.pos.w;"),
        "{wgsl}"
    );
    assert!(
        wgsl.contains("out.pos.y = out.pos.y + half_pixel.inv_viewport.y * out.pos.w;"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs_half_pixel_center_disabled_omits_uniform_and_adjustment() {
    // vs_2_0:
    //   mov oPos, v0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),     // oPos
        src_token(1, 0, 0xE4, 0), // v0
        0x0000_FFFF,              // end
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    assert!(
        !wgsl.contains("@group(3) @binding(0) var<uniform> half_pixel: HalfPixel;"),
        "{wgsl}"
    );
    assert!(
        !wgsl.contains("out.pos.x = out.pos.x - half_pixel.inv_viewport.x * out.pos.w;"),
        "{wgsl}"
    );
    assert!(
        !wgsl.contains("out.pos.y = out.pos.y + half_pixel.inv_viewport.y * out.pos.w;"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}
