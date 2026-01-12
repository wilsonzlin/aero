use std::collections::HashMap;

use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use crate::readback_rgba8;
use crate::shared_surface::SharedSurfaceTable;
use crate::texture_manager::TextureRegion;
use crate::GpuError;
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;

#[derive(Debug, thiserror::Error)]
pub enum AeroGpuAcmdExecutorError {
    #[error("failed to parse AeroGPU command stream: {0}")]
    Parse(#[from] AeroGpuCmdStreamParseError),
    #[error("unsupported texture format {format}: {reason}")]
    UnsupportedTextureFormat { format: u32, reason: String },
    #[error("unknown texture handle {0}")]
    UnknownTexture(u32),
    #[error("no render target bound")]
    NoRenderTarget,
    #[error("wgpu backend error: {0}")]
    Backend(String),
}

#[derive(Debug)]
struct Texture2d {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    is_x8: bool,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Minimal host-side executor for the versioned AeroGPU command stream (`aerogpu_cmd.h`).
///
/// This is intended as a wiring layer for the emulator so that guest submissions (ring +
/// `ACMD` command buffers) can result in actual command execution and scanout output.
///
/// The implementation is intentionally minimal and currently supports only a subset of commands
/// required for end-to-end integration tests (texture creation, render target binding, clear,
/// present, and readback).
pub struct AeroGpuAcmdExecutor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    textures: HashMap<u32, Texture2d>,
    render_target: u32,
    presented_scanouts: HashMap<u32, u32>,

    // D3D9Ex shared surface import/export bookkeeping (EXPORT/IMPORT_SHARED_SURFACE).
    shared_surfaces: SharedSurfaceTable,
}

fn is_x8_format(format: u32) -> bool {
    format == AerogpuFormat::B8G8R8X8Unorm as u32
        || format == AerogpuFormat::R8G8B8X8Unorm as u32
        || format == AerogpuFormat::B8G8R8X8UnormSrgb as u32
        || format == AerogpuFormat::R8G8B8X8UnormSrgb as u32
}

impl AeroGpuAcmdExecutor {
    pub async fn new_headless() -> Result<Self, GpuError> {
        // When using the GL backend on Linux, wgpu can emit noisy warnings if `XDG_RUNTIME_DIR` is
        // unset or points at a directory with unsafe permissions (e.g. `/tmp` is typically `1777`).
        // Create a per-process temp dir so headless callers don't need to care about display-server
        // environment details.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
                Ok(dir) if !dir.is_empty() => match std::fs::metadata(&dir) {
                    Ok(meta) => !meta.is_dir() || (meta.permissions().mode() & 0o077) != 0,
                    Err(_) => true,
                },
                _ => true,
            };
            if needs_runtime_dir {
                let dir = std::env::temp_dir().join(format!(
                    "aero-wgpu-xdg-runtime-{}-acmd-executor",
                    std::process::id()
                ));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        // On Linux CI we prefer the GL backend first to avoid crashes seen with some Vulkan
        // software adapters (lavapipe/llvmpipe). If no GL adapter is available, fall back to
        // the native backends.
        let adapter = if cfg!(target_os = "linux") {
            let gl_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::GL,
                ..Default::default()
            });
            let adapter = gl_instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await;
            if adapter.is_some() {
                adapter
            } else {
                let primary_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                    // Prefer "native" backends; this avoids initializing GL stacks on platforms
                    // where they're more likely to require a windowing system.
                    backends: wgpu::Backends::PRIMARY,
                    ..Default::default()
                });
                primary_instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        } else {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                // Prefer "native" backends; this avoids initializing GL stacks on platforms where
                // they're more likely to require a windowing system.
                backends: wgpu::Backends::PRIMARY,
                ..Default::default()
            });
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
        .ok_or_else(|| GpuError::Backend("no suitable wgpu adapter found".into()))?;

        let requested_features = crate::wgpu_features::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-gpu ACMD executor"),
                    required_features: requested_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|err| GpuError::Backend(err.to_string()))?;

        Ok(Self {
            device,
            queue,
            textures: HashMap::new(),
            render_target: 0,
            presented_scanouts: HashMap::new(),
            shared_surfaces: SharedSurfaceTable::default(),
        })
    }

    pub fn reset(&mut self) {
        self.textures.clear();
        self.render_target = 0;
        self.presented_scanouts.clear();
        self.shared_surfaces.clear();
    }

    pub fn execute_submission(
        &mut self,
        cmd_stream_bytes: &[u8],
        _alloc_table_bytes: Option<&[u8]>,
    ) -> Result<(), AeroGpuAcmdExecutorError> {
        if cmd_stream_bytes.is_empty() {
            return Ok(());
        }

        let stream = parse_cmd_stream(cmd_stream_bytes)?;
        for cmd in stream.cmds {
            match cmd {
                AeroGpuCmd::CreateTexture2d {
                    texture_handle,
                    format,
                    width,
                    height,
                    ..
                } => {
                    if self.textures.contains_key(&texture_handle) {
                        return Err(AeroGpuAcmdExecutorError::Backend(format!(
                            "CREATE_TEXTURE2D: texture_handle 0x{texture_handle:08X} is still in use"
                        )));
                    }

                    if width == 0 || height == 0 {
                        return Err(AeroGpuAcmdExecutorError::Backend(
                            "CREATE_TEXTURE2D: width/height must be non-zero".into(),
                        ));
                    }
                    let max_dim = self.device.limits().max_texture_dimension_2d;
                    if width > max_dim || height > max_dim {
                        return Err(AeroGpuAcmdExecutorError::Backend(format!(
                            "CREATE_TEXTURE2D: dimensions {width}x{height} exceed device limit {max_dim}"
                        )));
                    }

                    let format_raw = format;
                    let is_x8 = is_x8_format(format_raw);
                    let format = map_texture_format(format_raw)?;

                    let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("aerogpu texture2d"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::COPY_SRC
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });

                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    self.textures.insert(
                        texture_handle,
                        Texture2d {
                            width,
                            height,
                            format,
                            is_x8,
                            texture,
                            view,
                        },
                    );

                    if let Err(e) = self.shared_surfaces.register_handle(texture_handle) {
                        // Avoid leaking the newly-created texture if the handle is invalid
                        // (e.g. collides with an imported alias).
                        self.textures.remove(&texture_handle);
                        return Err(AeroGpuAcmdExecutorError::Backend(e.to_string()));
                    }
                }
                AeroGpuCmd::SetRenderTargets {
                    color_count,
                    colors,
                    ..
                } => {
                    let rt = if color_count == 0 { 0 } else { colors[0] };
                    self.render_target = self
                        .shared_surfaces
                        .resolve_cmd_handle(rt)
                        .map_err(|e| AeroGpuAcmdExecutorError::Backend(e.to_string()))?;
                }
                AeroGpuCmd::Clear {
                    flags,
                    color_rgba_f32,
                    ..
                } => {
                    if (flags & cmd::AEROGPU_CLEAR_COLOR) == 0 {
                        // Only color clear is currently supported.
                        continue;
                    }

                    let rt = self.render_target;
                    if rt == 0 {
                        return Err(AeroGpuAcmdExecutorError::NoRenderTarget);
                    }
                    let tex = self
                        .textures
                        .get(&rt)
                        .ok_or(AeroGpuAcmdExecutorError::UnknownTexture(rt))?;

                    let mut a = f32::from_bits(color_rgba_f32[3]);
                    if tex.is_x8 {
                        a = 1.0;
                    }
                    let color = wgpu::Color {
                        r: f32::from_bits(color_rgba_f32[0]) as f64,
                        g: f32::from_bits(color_rgba_f32[1]) as f64,
                        b: f32::from_bits(color_rgba_f32[2]) as f64,
                        a: a as f64,
                    };

                    let mut encoder =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("aerogpu clear encoder"),
                            });

                    {
                        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("aerogpu clear"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &tex.view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(color),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        });
                    }

                    self.queue.submit([encoder.finish()]);

                    #[cfg(not(target_arch = "wasm32"))]
                    self.device.poll(wgpu::Maintain::Wait);

                    #[cfg(target_arch = "wasm32")]
                    self.device.poll(wgpu::Maintain::Poll);
                }
                AeroGpuCmd::Present { scanout_id, .. }
                | AeroGpuCmd::PresentEx { scanout_id, .. } => {
                    // Minimal semantics: present the currently bound render target.
                    if self.render_target != 0 {
                        self.presented_scanouts
                            .insert(scanout_id, self.render_target);
                    }
                }
                AeroGpuCmd::ExportSharedSurface {
                    resource_handle,
                    share_token,
                } => {
                    self.shared_surfaces
                        .export(resource_handle, share_token)
                        .map_err(|e| AeroGpuAcmdExecutorError::Backend(e.to_string()))?;
                }
                AeroGpuCmd::ImportSharedSurface {
                    out_resource_handle,
                    share_token,
                } => {
                    self.shared_surfaces
                        .import(out_resource_handle, share_token)
                        .map_err(|e| AeroGpuAcmdExecutorError::Backend(e.to_string()))?;
                }
                AeroGpuCmd::ReleaseSharedSurface { share_token } => {
                    self.shared_surfaces.release_token(share_token);
                }
                AeroGpuCmd::DestroyResource { resource_handle } => {
                    if let Some((underlying, last_ref)) =
                        self.shared_surfaces.destroy_handle(resource_handle)
                    {
                        if last_ref {
                            self.textures.remove(&underlying);
                            if self.render_target == underlying {
                                self.render_target = 0;
                            }
                            self.presented_scanouts.retain(|_, v| *v != underlying);
                        }
                    } else {
                        self.textures.remove(&resource_handle);
                        if self.render_target == resource_handle {
                            self.render_target = 0;
                        }
                        self.presented_scanouts.retain(|_, v| *v != resource_handle);
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub async fn read_presented_scanout_rgba8(
        &self,
        scanout_id: u32,
    ) -> Result<Option<(u32, u32, Vec<u8>)>, AeroGpuAcmdExecutorError> {
        let Some(&tex_handle) = self.presented_scanouts.get(&scanout_id) else {
            return Ok(None);
        };
        let tex = self
            .textures
            .get(&tex_handle)
            .ok_or(AeroGpuAcmdExecutorError::UnknownTexture(tex_handle))?;

        // `readback_rgba8` is test-facing and assumes 4 bytes/pixel. Guard against any future
        // texture formats (e.g. BCn) that would otherwise panic inside wgpu's copy validation.
        let is_bgra = match tex.format {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
            other => {
                return Err(AeroGpuAcmdExecutorError::Backend(format!(
                "read_presented_scanout_rgba8 only supports RGBA/BGRA 8-bit textures (got {:?})",
                other
            )))
            }
        };

        let mut rgba8 = readback_rgba8(
            &self.device,
            &self.queue,
            &tex.texture,
            TextureRegion::full(wgpu::Extent3d {
                width: tex.width,
                height: tex.height,
                depth_or_array_layers: 1,
            }),
        )
        .await;

        if is_bgra {
            // Convert BGRA -> RGBA.
            for px in rgba8.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }

        if tex.is_x8 {
            // Report X8 textures as opaque, regardless of what wgpu stored in the underlying alpha
            // channel.
            for px in rgba8.chunks_exact_mut(4) {
                px[3] = 0xFF;
            }
        }

        Ok(Some((tex.width, tex.height, rgba8)))
    }
}

fn map_texture_format(format: u32) -> Result<wgpu::TextureFormat, AeroGpuAcmdExecutorError> {
    match format {
        x if x == AerogpuFormat::B8G8R8A8Unorm as u32 || x == AerogpuFormat::B8G8R8X8Unorm as u32 => {
            Ok(wgpu::TextureFormat::Bgra8Unorm)
        }
        x if x == AerogpuFormat::R8G8B8A8Unorm as u32 || x == AerogpuFormat::R8G8B8X8Unorm as u32 => {
            Ok(wgpu::TextureFormat::Rgba8Unorm)
        }
        x if x == AerogpuFormat::B8G8R8A8UnormSrgb as u32
            || x == AerogpuFormat::B8G8R8X8UnormSrgb as u32 =>
        {
            Ok(wgpu::TextureFormat::Bgra8UnormSrgb)
        }
        x if x == AerogpuFormat::R8G8B8A8UnormSrgb as u32
            || x == AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
        {
            Ok(wgpu::TextureFormat::Rgba8UnormSrgb)
        }

        // BCn compressed formats require special handling and cannot be used as render targets in
        // WebGPU, which this minimal executor relies on (CLEAR + PRESENT + readback).
        x if is_bc_format(x) => Err(AeroGpuAcmdExecutorError::UnsupportedTextureFormat {
            format: x,
            reason: "BC compressed texture formats are not supported by the ACMD executor (only BGRA/RGBA 8-bit render targets are supported)".into(),
        }),

        other => Err(AeroGpuAcmdExecutorError::UnsupportedTextureFormat {
            format: other,
            reason: "only BGRA/RGBA 8-bit textures (and their sRGB variants) are supported by the ACMD executor".into(),
        }),
    }
}

fn is_bc_format(format: u32) -> bool {
    format == AerogpuFormat::BC1RgbaUnorm as u32
        || format == AerogpuFormat::BC1RgbaUnormSrgb as u32
        || format == AerogpuFormat::BC2RgbaUnorm as u32
        || format == AerogpuFormat::BC2RgbaUnormSrgb as u32
        || format == AerogpuFormat::BC3RgbaUnorm as u32
        || format == AerogpuFormat::BC3RgbaUnormSrgb as u32
        || format == AerogpuFormat::BC7RgbaUnorm as u32
        || format == AerogpuFormat::BC7RgbaUnormSrgb as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
    use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

    #[test]
    fn map_texture_format_accepts_uncompressed_srgb_formats() {
        assert_eq!(
            map_texture_format(AerogpuFormat::B8G8R8A8UnormSrgb as u32).unwrap(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
        assert_eq!(
            map_texture_format(AerogpuFormat::B8G8R8X8UnormSrgb as u32).unwrap(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
        assert_eq!(
            map_texture_format(AerogpuFormat::R8G8B8A8UnormSrgb as u32).unwrap(),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            map_texture_format(AerogpuFormat::R8G8B8X8UnormSrgb as u32).unwrap(),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
    }

    #[test]
    fn map_texture_format_rejects_bc_formats_with_actionable_error() {
        let err = map_texture_format(AerogpuFormat::BC1RgbaUnorm as u32).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("BC"),
            "expected error message to mention BC, got: {message}"
        );
    }

    #[test]
    fn destroy_resource_clears_presented_scanout() {
        pollster::block_on(async {
            let mut exec = AeroGpuAcmdExecutor::new_headless().await.unwrap();

            const TEX: u32 = 1;
            const SCANOUT: u32 = 0;

            let mut w = AerogpuCmdWriter::new();
            w.create_texture2d(
                TEX,
                /*usage_flags=*/ 0,
                AerogpuFormat::B8G8R8A8Unorm as u32,
                /*width=*/ 4,
                /*height=*/ 4,
                /*mip_levels=*/ 1,
                /*array_layers=*/ 1,
                /*row_pitch_bytes=*/ 0,
                /*backing_alloc_id=*/ 0,
                /*backing_offset_bytes=*/ 0,
            );
            w.set_render_targets(&[TEX], /*depth_stencil=*/ 0);
            w.present(SCANOUT, /*flags=*/ 0);
            w.destroy_resource(TEX);

            exec.execute_submission(&w.finish(), None).unwrap();

            // The scanout should be cleared rather than pointing at a destroyed texture.
            let scanout = exec.read_presented_scanout_rgba8(SCANOUT).await.unwrap();
            assert!(
                scanout.is_none(),
                "expected scanout to be cleared after destroy"
            );
        });
    }

    #[test]
    fn set_render_targets_rejects_destroyed_original_handle_while_alias_alive() {
        pollster::block_on(async {
            let mut exec = AeroGpuAcmdExecutor::new_headless().await.unwrap();

            const ORIGINAL: u32 = 1;
            const ALIAS: u32 = 2;
            const TOKEN: u64 = 0x1122_3344_5566_7788;

            let mut w = AerogpuCmdWriter::new();
            w.create_texture2d(
                ORIGINAL,
                /*usage_flags=*/ 0,
                AerogpuFormat::B8G8R8A8Unorm as u32,
                /*width=*/ 4,
                /*height=*/ 4,
                /*mip_levels=*/ 1,
                /*array_layers=*/ 1,
                /*row_pitch_bytes=*/ 0,
                /*backing_alloc_id=*/ 0,
                /*backing_offset_bytes=*/ 0,
            );
            w.export_shared_surface(ORIGINAL, TOKEN);
            w.import_shared_surface(ALIAS, TOKEN);
            w.destroy_resource(ORIGINAL);

            // The original handle is now destroyed but the underlying texture is still alive via
            // the alias. The destroyed original handle ID must not be accepted for subsequent
            // commands.
            w.set_render_targets(&[ORIGINAL], /*depth_stencil=*/ 0);

            let err = exec.execute_submission(&w.finish(), None).unwrap_err();
            let message = err.to_string();
            assert!(
                message.contains("destroyed") || message.contains("was destroyed"),
                "unexpected error: {message}"
            );
        });
    }

    #[test]
    fn create_texture2d_rejects_zero_sized_textures() {
        pollster::block_on(async {
            let mut exec = AeroGpuAcmdExecutor::new_headless().await.unwrap();

            let mut w = AerogpuCmdWriter::new();
            w.create_texture2d(
                /*texture_handle=*/ 1,
                /*usage_flags=*/ 0,
                AerogpuFormat::B8G8R8A8Unorm as u32,
                /*width=*/ 0,
                /*height=*/ 4,
                /*mip_levels=*/ 1,
                /*array_layers=*/ 1,
                /*row_pitch_bytes=*/ 0,
                /*backing_alloc_id=*/ 0,
                /*backing_offset_bytes=*/ 0,
            );

            let err = exec.execute_submission(&w.finish(), None).unwrap_err();
            assert!(
                err.to_string().contains("width/height"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn clear_x8_render_target_forces_opaque_alpha() {
        pollster::block_on(async {
            let mut exec = AeroGpuAcmdExecutor::new_headless().await.unwrap();

            const SCANOUT: u32 = 0;
            let cases = [
                ("rgba_x8", AerogpuFormat::R8G8B8X8Unorm as u32),
                ("bgra_x8", AerogpuFormat::B8G8R8X8Unorm as u32),
            ];

            for (idx, (label, format)) in cases.into_iter().enumerate() {
                exec.reset();

                let tex = 1 + idx as u32;
                let mut w = AerogpuCmdWriter::new();
                w.create_texture2d(
                    tex, /*usage_flags=*/ 0, format, /*width=*/ 1, /*height=*/ 1,
                    /*mip_levels=*/ 1, /*array_layers=*/ 1, /*row_pitch_bytes=*/ 0,
                    /*backing_alloc_id=*/ 0, /*backing_offset_bytes=*/ 0,
                );
                w.set_render_targets(&[tex], /*depth_stencil=*/ 0);
                w.clear(
                    cmd::AEROGPU_CLEAR_COLOR,
                    [0.0, 0.0, 0.0, 0.0], // alpha should be ignored for X8 formats
                    /*depth=*/ 1.0,
                    /*stencil=*/ 0,
                );
                w.present(SCANOUT, /*flags=*/ 0);

                exec.execute_submission(&w.finish(), None)
                    .unwrap_or_else(|e| panic!("{label}: execute_submission failed: {e:?}"));

                let scanout = exec
                    .read_presented_scanout_rgba8(SCANOUT)
                    .await
                    .unwrap_or_else(|e| {
                        panic!("{label}: read_presented_scanout_rgba8 failed: {e:?}")
                    })
                    .expect("{label}: scanout should exist after present");
                assert_eq!((scanout.0, scanout.1), (1, 1), "{label}");
                assert_eq!(&scanout.2[0..4], &[0, 0, 0, 255], "{label}");
            }
        });
    }
}
