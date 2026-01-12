mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
        AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_MAJOR},
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);
const AEROGPU_ABI_VERSION_U32_COMPAT: u32 = AEROGPU_ABI_MAJOR << 16; // minor=0

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn align4(v: usize) -> usize {
    (v + 3) & !3
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32_COMPAT);
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);
    let end_aligned = align4(out.len());
    out.resize(end_aligned, 0);
    let size_bytes = (end_aligned - start) as u32;
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

// Protocol constants are sourced from `aero-protocol` to avoid drift.
const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
const OPC_DESTROY_RESOURCE: u32 = AerogpuCmdOpcode::DestroyResource as u32;
const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
const OPC_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;
const OPC_EXPORT_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ExportSharedSurface as u32;
const OPC_IMPORT_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ImportSharedSurface as u32;
const OPC_RELEASE_SHARED_SURFACE: u32 = AerogpuCmdOpcode::ReleaseSharedSurface as u32;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;

fn emit_create_texture2d_rgba8(out: &mut Vec<u8>, handle: u32, width: u32, height: u32) {
    emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
        push_u32(out, handle);
        push_u32(
            out,
            AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        );
        push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
        push_u32(out, width);
        push_u32(out, height);
        push_u32(out, 1); // mip_levels
        push_u32(out, 1); // array_layers
        push_u32(out, width * 4); // row_pitch_bytes
        push_u32(out, 0); // backing_alloc_id
        push_u32(out, 0); // backing_offset_bytes
        push_u64(out, 0); // reserved0
    });
}

#[test]
fn d3d9_cmd_stream_shared_surface_alias_survives_original_destroy() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 1;
    const TEX_ALIAS_A: u32 = 2;
    const TEX_ALIAS_B: u32 = 3;

    const TOKEN_A: u64 = 0x1122_3344_5566_7788;
    const TOKEN_B: u64 = 0x8877_6655_4433_2211;

    let width = 4u32;
    let height = 4u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN_A);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS_A);
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN_A);
        });

        // Exporting an alias should resolve to the underlying resource.
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS_A);
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN_B);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS_B);
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN_B);
        });

        // Drop the original handle; the alias should keep the underlying texture alive.
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX_ALIAS_B);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Clear to solid red.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(TEX_ALIAS_B))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    let idx = ((2 * width + 2) * 4) as usize;
    assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_release_shared_surface_invalidates_token_but_keeps_existing_alias_alive() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 0x10;
    const TEX_ALIAS_A: u32 = 0x20;
    const TEX_ALIAS_B: u32 = 0x21;

    const TOKEN: u64 = 0xAABB_CCDD_EEFF_0123;

    let width = 4u32;
    let height = 4u32;

    // Create + export + import alias, then destroy the original handle. Alias remains alive.
    let submit1 = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS_A);
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });

        // Drop the original handle so only the alias keeps the resource alive.
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX_ALIAS_A);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Clear to solid red.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });
    exec.execute_cmd_stream(&submit1)
        .expect("submission 1 should succeed");

    let (_w, _h, rgba) = pollster::block_on(exec.readback_texture_rgba8(TEX_ALIAS_A))
        .expect("readback should succeed");
    let idx = ((2 * width + 2) * 4) as usize;
    assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);

    // Explicitly release the token mapping and ensure future imports fail, while the existing alias
    // continues to function.
    let submit2 = build_stream(|out| {
        emit_packet(out, OPC_RELEASE_SHARED_SURFACE, |out| {
            push_u64(out, TOKEN);
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS_B);
            push_u32(out, 0); // reserved0
            push_u64(out, TOKEN);
        });
    });
    let err = exec
        .execute_cmd_stream(&submit2)
        .expect_err("import after RELEASE_SHARED_SURFACE should fail");
    assert!(matches!(
        err,
        AerogpuD3d9Error::UnknownShareToken(t) if t == TOKEN
    ));

    let submit3 = build_stream(|out| {
        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX_ALIAS_A);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Clear to solid green.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });
    exec.execute_cmd_stream(&submit3)
        .expect("submission 3 should succeed");

    let (_w, _h, rgba) = pollster::block_on(exec.readback_texture_rgba8(TEX_ALIAS_A))
        .expect("readback should succeed");
    let idx = ((2 * width + 2) * 4) as usize;
    assert_eq!(&rgba[idx..idx + 4], &[0, 255, 0, 255]);

    let teardown = build_stream(|out| {
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ALIAS_A);
            push_u32(out, 0); // reserved0
        });
    });
    exec.execute_cmd_stream(&teardown)
        .expect("teardown should succeed");
}

#[test]
fn d3d9_cmd_stream_release_shared_surface_unknown_token_is_noop() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 0x10;
    const TEX_ALIAS: u32 = 0x20;
    const TOKEN: u64 = 0x1111_2222_3333_4444;

    let width = 4u32;
    let height = 4u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);

        // Token has not been exported yet; this should be a no-op and must not retire the token.
        emit_packet(out, OPC_RELEASE_SHARED_SURFACE, |out| {
            push_u64(out, TOKEN);
            push_u64(out, 0);
        });

        // Export/import should still succeed.
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX_ALIAS);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Clear to solid green.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(TEX_ALIAS))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    let idx = ((2 * width + 2) * 4) as usize;
    assert_eq!(&rgba[idx..idx + 4], &[0, 255, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_reusing_underlying_handle_while_alias_alive_is_an_error() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 0x10;
    const TEX_ALIAS: u32 = 0x20;
    const TOKEN: u64 = 0xAABB_CCDD_EEFF_0001;

    let width = 1u32;
    let height = 1u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        // Destroy the original handle; the alias keeps the underlying allocation alive.
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
        });

        // Reusing the now-destroyed original handle would overwrite the underlying resource entry,
        // corrupting any remaining aliases. Treat this as a validation error.
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);
    });

    let err = exec
        .execute_cmd_stream(&stream)
        .expect_err("handle reuse should be rejected");
    assert!(matches!(
        err,
        AerogpuD3d9Error::ResourceHandleInUse(TEX_ORIGINAL)
    ));
}

#[test]
fn d3d9_cmd_stream_import_is_idempotent_and_token_invalid_after_last_handle_destroyed() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 0x10;
    const TEX_ALIAS: u32 = 0x20;
    const TEX_REIMPORT: u32 = 0x30;
    const TOKEN: u64 = 0xDEAD_BEEF_CAFE_F00D;

    let width = 1u32;
    let height = 1u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        // Importing into the same alias again is idempotent and must not leak a refcount.
        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
        });

        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ALIAS);
            push_u32(out, 0);
        });

        // Now that the last handle is gone, the token should be invalidated.
        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_REIMPORT);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let err = exec
        .execute_cmd_stream(&stream)
        .expect_err("import after destroying the last handle should fail");
    assert!(matches!(err, AerogpuD3d9Error::UnknownShareToken(t) if t == TOKEN));
}

#[test]
fn d3d9_cmd_stream_exporting_same_token_for_different_resources_is_an_error() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_A: u32 = 0x10;
    const TEX_B: u32 = 0x11;
    const TOKEN: u64 = 0x1111_2222_3333_4444;
    let width = 1u32;
    let height = 1u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_A, width, height);
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_A);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_create_texture2d_rgba8(out, TEX_B, width, height);
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_B);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let err = exec
        .execute_cmd_stream(&stream)
        .expect_err("token reuse should fail validation");
    assert!(matches!(
        err,
        AerogpuD3d9Error::ShareTokenAlreadyExported {
            share_token,
            existing: TEX_A,
            new: TEX_B
        } if share_token == TOKEN
    ));
}

#[test]
fn d3d9_cmd_stream_token_reuse_after_release_is_rejected_on_export() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_A: u32 = 0x10;
    const TEX_B: u32 = 0x11;
    const TOKEN: u64 = 0x1111_2222_3333_4444;
    let width = 1u32;
    let height = 1u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_A, width, height);
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_A);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_RELEASE_SHARED_SURFACE, |out| {
            push_u64(out, TOKEN);
            push_u64(out, 0);
        });

        emit_create_texture2d_rgba8(out, TEX_B, width, height);
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_B);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });

    let err = exec
        .execute_cmd_stream(&stream)
        .expect_err("token reuse after RELEASE_SHARED_SURFACE should be rejected");
    assert!(matches!(err, AerogpuD3d9Error::ShareTokenRetired(t) if t == TOKEN));
}

#[test]
fn d3d9_cmd_stream_importing_into_existing_alias_for_different_original_is_an_error() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_A: u32 = 0x10;
    const TEX_B: u32 = 0x11;
    const ALIAS: u32 = 0x20;
    const TOKEN_A: u64 = 0xAAA0_0000_0000_0001;
    const TOKEN_B: u64 = 0xBBB0_0000_0000_0002;
    let width = 1u32;
    let height = 1u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_A, width, height);
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_A);
            push_u32(out, 0);
            push_u64(out, TOKEN_A);
        });

        emit_create_texture2d_rgba8(out, TEX_B, width, height);
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_B);
            push_u32(out, 0);
            push_u64(out, TOKEN_B);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN_A);
        });

        // Attempt to bind the same alias handle to a different underlying surface.
        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN_B);
        });
    });

    let err = exec
        .execute_cmd_stream(&stream)
        .expect_err("alias retarget should fail validation");
    assert!(matches!(
        err,
        AerogpuD3d9Error::SharedSurfaceAliasAlreadyBound {
            alias: ALIAS,
            existing: TEX_A,
            new: TEX_B
        }
    ));
}

#[test]
fn d3d9_cmd_stream_destroying_alias_keeps_token_importable() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 0x10;
    const TEX_ALIAS_A: u32 = 0x20;
    const TEX_ALIAS_B: u32 = 0x21;
    const TOKEN: u64 = 0xAABB_CCDD_EEFF_0123;

    let width = 4u32;
    let height = 4u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS_A);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ALIAS_A);
            push_u32(out, 0);
        });

        // Token should remain importable as long as the original handle is still alive.
        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS_B);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX_ALIAS_B);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Clear to solid green.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(TEX_ALIAS_B))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    let idx = ((2 * width + 2) * 4) as usize;
    assert_eq!(&rgba[idx..idx + 4], &[0, 255, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_export_in_one_stream_import_in_next_stream_succeeds() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 0x10;
    const TEX_ALIAS: u32 = 0x20;
    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let width = 4u32;
    let height = 4u32;

    let submit1 = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
    });
    exec.execute_cmd_stream(&submit1)
        .expect("submission 1 should succeed");

    let submit2 = build_stream(|out| {
        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX_ALIAS);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Clear to solid blue.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });
    exec.execute_cmd_stream(&submit2)
        .expect("submission 2 should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(TEX_ALIAS))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    let idx = ((2 * width + 2) * 4) as usize;
    assert_eq!(&rgba[idx..idx + 4], &[0, 0, 255, 255]);
}

#[test]
fn d3d9_cmd_stream_presented_scanout_survives_original_destroy_when_alias_keeps_alive() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX_ORIGINAL: u32 = 0x10;
    const TEX_ALIAS: u32 = 0x20;
    const TOKEN: u64 = 0x1122_3344_5566_7788;

    let width = 4u32;
    let height = 4u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX_ORIGINAL, width, height);

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });
        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, TEX_ALIAS);
            push_u32(out, 0);
            push_u64(out, TOKEN);
        });

        // Present using the original handle, then destroy it. The alias should keep the underlying
        // resource alive, and `read_presented_scanout_rgba8` should still work.
        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX_ORIGINAL);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });
        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX_ORIGINAL);
            push_u32(out, 0);
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.read_presented_scanout_rgba8(0))
        .expect("scanout readback should succeed")
        .expect("scanout should be present");
    assert_eq!((out_w, out_h), (width, height));

    let idx = ((2 * width + 2) * 4) as usize;
    assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);
}

#[test]
fn d3d9_cmd_stream_destroying_last_handle_clears_presented_scanout() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    const TEX: u32 = 0x10;
    let width = 4u32;
    let height = 4u32;

    let stream = build_stream(|out| {
        emit_create_texture2d_rgba8(out, TEX, width, height);

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, TEX);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Clear to solid red.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });

        // Destroy the last handle to the resource; the scanout mapping should be cleared to avoid
        // dangling references.
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, TEX);
            push_u32(out, 0);
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let scanout = pollster::block_on(exec.read_presented_scanout_rgba8(0))
        .expect("read_presented_scanout should succeed");
    assert!(scanout.is_none(), "scanout mapping should be cleared");
}
