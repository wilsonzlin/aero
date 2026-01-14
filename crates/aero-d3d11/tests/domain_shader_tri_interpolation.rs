mod common;

use aero_d3d11::sm4::{decode_program, opcode::*};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, DxbcSignatureParameter, FourCC,
    Sm4Program, Swizzle, WriteMask,
};
use anyhow::{anyhow, Context, Result};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_PSGN: FourCC = FourCC(*b"PSGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

// D3D_NAME values used by the token stream declarations.
const D3D_NAME_POSITION: u32 = 1;
const D3D_NAME_PRIMITIVE_ID: u32 = 7;
const D3D_NAME_DOMAIN_LOCATION: u32 = 12;

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks");
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
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes());
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
    bytes
}

fn sig_param(name: &str, index: u32, register: u32, mask: u8) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type: 0,
        component_type: 0,
        register,
        mask,
        read_write_mask: mask,
        stream: 0,
        min_precision: 0,
    }
}

fn build_signature_chunk(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    // Same layout as D3D10+ signature chunks:
    // header: u32 param_count, u32 param_offset
    // table entries: 24 bytes each
    let param_count = u32::try_from(params.len()).expect("too many signature params");
    let header_len = 8usize;
    let entry_size = 24usize;
    let table_len = params.len() * entry_size;

    let mut strings = Vec::<u8>::new();
    let mut name_offsets = Vec::<u32>::with_capacity(params.len());
    for p in params {
        name_offsets.push((header_len + table_len + strings.len()) as u32);
        strings.extend_from_slice(p.semantic_name.as_bytes());
        strings.push(0);
    }

    let mut bytes = Vec::with_capacity(header_len + table_len + strings.len());
    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&(header_len as u32).to_le_bytes());

    for (p, &name_off) in params.iter().zip(name_offsets.iter()) {
        bytes.extend_from_slice(&name_off.to_le_bytes());
        bytes.extend_from_slice(&p.semantic_index.to_le_bytes());
        bytes.extend_from_slice(&p.system_value_type.to_le_bytes());
        bytes.extend_from_slice(&p.component_type.to_le_bytes());
        bytes.extend_from_slice(&p.register.to_le_bytes());
        bytes.push(p.mask);
        bytes.push(p.read_write_mask);
        bytes.push(p.stream);
        bytes.push(p.min_precision);
    }
    bytes.extend_from_slice(&strings);
    bytes
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    let version = ((stage_type as u32) << 16) | (5u32 << 4);
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
    extended: bool,
) -> u32 {
    let mut token = 0u32;
    token |= num_components & OPERAND_NUM_COMPONENTS_MASK;
    token |= (selection_mode & OPERAND_SELECTION_MODE_MASK) << OPERAND_SELECTION_MODE_SHIFT;
    token |= (ty & OPERAND_TYPE_MASK) << OPERAND_TYPE_SHIFT;
    token |=
        (component_sel & OPERAND_COMPONENT_SELECTION_MASK) << OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & OPERAND_INDEX_DIMENSION_MASK) << OPERAND_INDEX_DIMENSION_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX0_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX1_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX2_REP_SHIFT;
    if extended {
        token |= OPERAND_EXTENDED_BIT;
    }
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1, false),
        idx,
    ]
}

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle) -> Vec<u32> {
    let num_components = match ty {
        OPERAND_TYPE_SAMPLER | OPERAND_TYPE_RESOURCE => 0,
        _ => 2,
    };
    let selection_mode = if num_components == 0 {
        OPERAND_SEL_MASK
    } else {
        OPERAND_SEL_SWIZZLE
    };
    let token = operand_token(
        ty,
        num_components,
        selection_mode,
        swizzle_bits(swizzle.0),
        indices.len() as u32,
        false,
    );
    let mut out = Vec::new();
    out.push(token);
    out.extend_from_slice(indices);
    out
}

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue, bool)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-d3d11-domain-shader-test-xdg-runtime-{}",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::PRIMARY
        },
        ..Default::default()
    });

    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })
        .await
    {
        Some(adapter) => Some(adapter),
        None => {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
    }
    .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

    let supports_compute = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 domain shader test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue, supports_compute))
}

async fn read_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    size: u64,
) -> Result<Vec<u8>> {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("domain shader readback staging"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("domain shader readback encoder"),
    });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });

    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);
    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);

    receiver
        .receive()
        .await
        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
        .context("wgpu: map_async failed")?;

    let data = slice.get_mapped_range().to_vec();
    staging.unmap();
    Ok(data)
}

#[test]
fn wgpu_domain_shader_tri_interpolates_control_points() {
    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return Ok(());
        }

        // Minimal ds_5_0 shader:
        //   o0 = v0[0] * dl.x + v0[1] * dl.y + v0[2] * dl.z
        // where dl = SV_DomainLocation (barycentric for tri domain).
        const DCL_INPUT: u32 = 0x100;
        const DCL_OUTPUT: u32 = 0x101;

        let mut body = Vec::<u32>::new();

        // dcl_input v0.xyzw
        body.push(opcode_token(DCL_INPUT, 3));
        body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));

        // dcl_input_siv v1.xyz, D3D_NAME_DOMAIN_LOCATION
        body.push(opcode_token(DCL_INPUT, 4));
        body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 1, WriteMask(0b0111)));
        body.push(D3D_NAME_DOMAIN_LOCATION);

        // dcl_input_siv v2.x, D3D_NAME_PRIMITIVE_ID
        body.push(opcode_token(DCL_INPUT, 4));
        body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 2, WriteMask(0b0001)));
        body.push(D3D_NAME_PRIMITIVE_ID);

        // dcl_output_siv o0.xyzw, D3D_NAME_POSITION
        body.push(opcode_token(DCL_OUTPUT, 4));
        body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        body.push(D3D_NAME_POSITION);

        let dst_r0 = reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW);
        let src_v0_0 = reg_src(OPERAND_TYPE_INPUT, &[0, 0], Swizzle::XYZW);
        let src_v0_1 = reg_src(OPERAND_TYPE_INPUT, &[0, 1], Swizzle::XYZW);
        let src_v0_2 = reg_src(OPERAND_TYPE_INPUT, &[0, 2], Swizzle::XYZW);
        let src_dl_x = reg_src(OPERAND_TYPE_INPUT, &[1], Swizzle::XXXX);
        let src_dl_y = reg_src(OPERAND_TYPE_INPUT, &[1], Swizzle::YYYY);
        let src_dl_z = reg_src(OPERAND_TYPE_INPUT, &[1], Swizzle::ZZZZ);
        let src_r0 = reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW);

        // mul r0, v0[0], v1.xxxx
        body.push(opcode_token(
            OPCODE_MUL,
            1 + dst_r0.len() as u32 + src_v0_0.len() as u32 + src_dl_x.len() as u32,
        ));
        body.extend_from_slice(&dst_r0);
        body.extend_from_slice(&src_v0_0);
        body.extend_from_slice(&src_dl_x);

        // mad r0, v0[1], v1.yyyy, r0
        body.push(opcode_token(
            OPCODE_MAD,
            1 + dst_r0.len() as u32
                + src_v0_1.len() as u32
                + src_dl_y.len() as u32
                + src_r0.len() as u32,
        ));
        body.extend_from_slice(&dst_r0);
        body.extend_from_slice(&src_v0_1);
        body.extend_from_slice(&src_dl_y);
        body.extend_from_slice(&src_r0);

        // mad r0, v0[2], v1.zzzz, r0
        body.push(opcode_token(
            OPCODE_MAD,
            1 + dst_r0.len() as u32
                + src_v0_2.len() as u32
                + src_dl_z.len() as u32
                + src_r0.len() as u32,
        ));
        body.extend_from_slice(&dst_r0);
        body.extend_from_slice(&src_v0_2);
        body.extend_from_slice(&src_dl_z);
        body.extend_from_slice(&src_r0);

        // mov o0, r0
        body.push(opcode_token(OPCODE_MOV, 1 + 2 + 2));
        body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
        body.extend_from_slice(&src_r0);

        // ret
        body.push(opcode_token(OPCODE_RET, 1));

        // Stage type 4 = domain shader.
        let tokens = make_sm5_program_tokens(4, &body);
        let dxbc_bytes = build_dxbc(&[
            (FOURCC_SHEX, tokens_to_bytes(&tokens)),
            (
                FOURCC_ISGN,
                build_signature_chunk(&[
                    // Indexed control-point register file (v0[cp_id]).
                    sig_param("CP", 0, 0, 0b1111),
                    sig_param("SV_DomainLocation", 0, 1, 0b0111),
                    sig_param("SV_PrimitiveID", 0, 2, 0b0001),
                ]),
            ),
            // Patch constant signature (unused by this shader, but required by the translator).
            (FOURCC_PSGN, build_signature_chunk(&[])),
            (
                FOURCC_OSGN,
                build_signature_chunk(&[sig_param("SV_Position", 0, 0, 0b1111)]),
            ),
        ]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).context("DXBC parse")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("SM4 parse")?;
        let module = decode_program(&program).context("SM4 decode")?;
        let signatures = parse_signatures(&dxbc).context("parse signatures")?;

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).context("translate")?;

        // Domain shader inputs are provided via internal storage buffers:
        // - `ds_in_cp`: HS output control points (vec4<f32> registers)
        // - `ds_in_pc`: HS patch constants (unused here)
        // - `ds_tess_factors`: tess factors (float scalars) used by the tessellator emulation
        //
        // Configure tess factor so the translator-derived `SV_DomainLocation` hits:
        //   (u, v, w) = (0.25, 0.25, 0.5)
        // which corresponds to tess_level=4 and vert_in_patch=6 (row=1, col=1).
        let tess_level: f32 = 4.0;
        let verts_per_patch: u32 = (4 + 1) * (4 + 2) / 2;
        assert_eq!(verts_per_patch, 15);

        // HS output control points: 3 control points, 1 register each.
        let cp0 = [1.0f32, 0.0, 0.0, 1.0];
        let cp1 = [0.0f32, 1.0, 0.0, 1.0];
        let cp2 = [0.0f32, 0.0, 1.0, 1.0];
        // The translator indexes control points with a fixed DS_MAX_CONTROL_POINTS=32 stride.
        // Populate the first 3 entries and leave the rest zeroed.
        let mut cp_bytes = Vec::<u8>::with_capacity(32 * 16);
        for cp in [cp0, cp1, cp2] {
            for f in cp {
                cp_bytes.extend_from_slice(&f.to_le_bytes());
            }
        }
        cp_bytes.resize(32 * 16, 0);

        let ds_in_cp = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ds test ds_in_cp"),
            size: cp_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ds_in_cp, 0, &cp_bytes);

        let ds_in_pc = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ds test ds_in_pc"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ds_in_pc, 0, &[0u8; 16]);

        // `ds_tess_factors`: 4 scalars per patch (outer[3] + inner[1]).
        let mut tess_bytes = Vec::<u8>::with_capacity(16);
        tess_bytes.extend_from_slice(&tess_level.to_le_bytes());
        tess_bytes.extend_from_slice(&0.0f32.to_le_bytes());
        tess_bytes.extend_from_slice(&0.0f32.to_le_bytes());
        tess_bytes.extend_from_slice(&0.0f32.to_le_bytes());
        let ds_tess_factors = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ds test ds_tess_factors"),
            size: tess_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ds_tess_factors, 0, &tess_bytes);

        // Output buffer: 15 vertices * 16 bytes (one `vec4<f32>` SV_Position per vertex).
        let ds_out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ds test ds_out"),
            size: verts_per_patch as u64 * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ds test shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ds test bgl"),
            entries: &[
                // @binding(0): ds_in_cp
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // @binding(1): ds_in_pc
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // @binding(2): ds_out
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // @binding(3): ds_tess_factors
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ds test bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ds_in_cp.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: ds_in_pc.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ds_out.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: ds_tess_factors.as_entire_binding(),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ds test pipeline layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ds test pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "ds_main",
            compilation_options: Default::default(),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ds test encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ds test pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);

        let out_bytes = read_buffer(&device, &queue, &ds_out, verts_per_patch as u64 * 16)
            .await
            .context("read back ds_out")?;
        assert_eq!(out_bytes.len(), verts_per_patch as usize * 16);

        let vertex_index = 6usize;
        let base = vertex_index * 16;
        let out_f = [
            f32::from_le_bytes(out_bytes[base..base + 4].try_into().unwrap()),
            f32::from_le_bytes(out_bytes[base + 4..base + 8].try_into().unwrap()),
            f32::from_le_bytes(out_bytes[base + 8..base + 12].try_into().unwrap()),
            f32::from_le_bytes(out_bytes[base + 12..base + 16].try_into().unwrap()),
        ];

        assert_eq!(out_f, [0.25, 0.25, 0.5, 1.0]);

        Ok::<_, anyhow::Error>(())
    })
    .unwrap();
}
