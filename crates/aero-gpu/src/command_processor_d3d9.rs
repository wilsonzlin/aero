//! Host-side D3D9 command processor (experimental).
//!
//! This module decodes the byte-oriented command stream defined in
//! [`crate::protocol_d3d9`] and drives a tiny `aero-d3d9` runtime backed by
//! WebGPU via `wgpu`.
//!
//! It exists primarily to provide an end-to-end smoke test path from a command
//! stream to WebGPU execution.

use std::collections::{HashMap, HashSet};

use aero_d3d9::runtime::{
    ColorFormat, D3D9Runtime, IndexFormat as RuntimeIndexFormat, RenderTarget, RuntimeConfig,
    ShaderStage as RuntimeShaderStage, SwapChainDesc, TextureDesc,
    TextureFormat as RuntimeTextureFormat, VertexAttributeDesc, VertexDecl,
    VertexFormat as RuntimeVertexFormat,
};
use aero_d3d9::state::tracker::{ScissorRect, Viewport};
use tracing::{debug, warn};

use crate::protocol_d3d9::{
    IndexFormat, Opcode, ShaderStage, TextureFormat, VertexFormat, COMMAND_HEADER_LEN,
    STREAM_HEADER_LEN, STREAM_MAGIC, STREAM_VERSION_MAJOR,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessorConfig {
    pub validation: bool,
}

#[derive(Debug, Clone)]
pub enum ProcessorEvent {
    Error { at: usize, message: String },
    FenceSignaled { fence_id: u32, value: u64 },
}

#[derive(Debug, Clone)]
pub struct ProcessReport {
    pub commands_processed: u32,
    pub events: Vec<ProcessorEvent>,
}

impl ProcessReport {
    pub fn is_ok(&self) -> bool {
        !self
            .events
            .iter()
            .any(|e| matches!(e, ProcessorEvent::Error { .. }))
    }
}

pub struct CommandProcessor {
    config: ProcessorConfig,
    devices: HashMap<u32, DeviceEntry>,
    contexts: HashMap<u32, u32>,

    shared_textures: SharedTextureState,
}

struct DeviceEntry {
    runtime: D3D9Runtime,
}

#[derive(Default)]
struct SharedTextureState {
    /// `share_token -> underlying texture handle`.
    shared_surface_by_token: HashMap<u64, u32>,
    /// `share_token` values that were previously valid but were released (or removed after the
    /// underlying texture was destroyed).
    ///
    /// Prevents misbehaving guests from "re-arming" a released token by re-exporting it for a
    /// different resource.
    retired_share_tokens: HashSet<u64>,
    /// `texture handle -> underlying texture handle`.
    ///
    /// Includes both original handles (identity mapping) and imported aliases.
    texture_handles: HashMap<u32, u32>,
    /// `underlying texture handle -> live handle refcount` (original + aliases).
    texture_refcounts: HashMap<u32, u32>,
}

impl SharedTextureState {
    fn retire_tokens_for_underlying(&mut self, underlying: u32) {
        let to_retire: Vec<u64> = self
            .shared_surface_by_token
            .iter()
            .filter_map(|(k, v)| (*v == underlying).then_some(*k))
            .collect();
        for token in to_retire {
            self.shared_surface_by_token.remove(&token);
            self.retired_share_tokens.insert(token);
        }
    }

    fn clear(&mut self) {
        self.shared_surface_by_token.clear();
        self.retired_share_tokens.clear();
        self.texture_handles.clear();
        self.texture_refcounts.clear();
    }

    fn resolve_texture_handle(&self, handle: u32) -> Option<u32> {
        self.texture_handles.get(&handle).copied()
    }

    fn register_texture_handle(&mut self, handle: u32, underlying: u32) -> Result<(), String> {
        if handle == 0 {
            return Err("texture handle 0 is reserved".into());
        }
        if self.texture_handles.contains_key(&handle) {
            return Err(format!("texture handle {handle} already exists"));
        }
        if self.texture_refcounts.contains_key(&handle) {
            // Underlying handles remain reserved as long as any aliases still reference them. If an
            // original handle was destroyed, reject reusing its numeric ID until the underlying
            // texture is fully released.
            return Err(format!(
                "texture handle {handle} is still in use (underlying id kept alive by shared surface aliases)"
            ));
        }
        self.texture_handles.insert(handle, underlying);
        *self.texture_refcounts.entry(underlying).or_insert(0) += 1;
        Ok(())
    }

    fn destroy_texture_handle(
        &mut self,
        runtime: &mut D3D9Runtime,
        handle: u32,
    ) -> Result<(), String> {
        let underlying = match self.texture_handles.remove(&handle) {
            Some(underlying) => underlying,
            None => {
                // If the original handle has already been destroyed (removed from `texture_handles`)
                // but aliases keep the underlying texture alive (`texture_refcounts` still contains
                // the underlying ID), treat duplicate destroys as an idempotent no-op.
                if self.texture_refcounts.contains_key(&handle) {
                    return Ok(());
                }
                return Err(format!("unknown texture handle {handle}"));
            }
        };

        let Some(count) = self.texture_refcounts.get_mut(&underlying) else {
            return Err(format!(
                "internal error: missing refcount entry for texture {underlying}"
            ));
        };

        *count = count.saturating_sub(1);
        if *count == 0 {
            self.texture_refcounts.remove(&underlying);
            runtime
                .destroy_texture(underlying)
                .map_err(|e| e.to_string())?;
            self.retire_tokens_for_underlying(underlying);
        }

        Ok(())
    }

    fn export_shared_surface(
        &mut self,
        resource_handle: u32,
        share_token: u64,
    ) -> Result<(), String> {
        if resource_handle == 0 {
            return Err("ExportSharedSurface resource_handle 0 is reserved".into());
        }
        if share_token == 0 {
            return Err("ExportSharedSurface share_token 0 is reserved".into());
        }
        if self.retired_share_tokens.contains(&share_token) {
            return Err(format!(
                "shared surface token 0x{share_token:016X} was previously released and cannot be reused"
            ));
        }
        let underlying = self
            .resolve_texture_handle(resource_handle)
            .ok_or_else(|| {
                format!("ExportSharedSurface references unknown texture handle {resource_handle}")
            })?;
        if let Some(existing) = self.shared_surface_by_token.get(&share_token).copied() {
            if existing != underlying {
                return Err(format!(
                    "shared surface token 0x{share_token:016X} already exported (existing_handle={existing} new_handle={underlying})"
                ));
            }
        } else {
            self.shared_surface_by_token.insert(share_token, underlying);
        }
        Ok(())
    }

    fn import_shared_surface(
        &mut self,
        out_resource_handle: u32,
        share_token: u64,
    ) -> Result<(), String> {
        if out_resource_handle == 0 {
            return Err("ImportSharedSurface out_resource_handle 0 is reserved".into());
        }
        if share_token == 0 {
            return Err("ImportSharedSurface share_token 0 is reserved".into());
        }
        let Some(&underlying) = self.shared_surface_by_token.get(&share_token) else {
            return Err(format!("unknown shared surface token 0x{share_token:016X}"));
        };

        if !self.texture_refcounts.contains_key(&underlying) {
            return Err(format!(
                "shared surface token 0x{share_token:016X} refers to destroyed texture {underlying}"
            ));
        }

        if let Some(existing) = self.texture_handles.get(&out_resource_handle).copied() {
            if existing != underlying {
                return Err(format!(
                    "texture handle {out_resource_handle} already exists (existing_handle={existing} new_handle={underlying})"
                ));
            }
            Ok(())
        } else {
            self.register_texture_handle(out_resource_handle, underlying)
        }
    }
}

impl CommandProcessor {
    pub fn new(config: ProcessorConfig) -> Self {
        Self {
            config,
            devices: HashMap::new(),
            contexts: HashMap::new(),
            shared_textures: SharedTextureState::default(),
        }
    }

    pub async fn process(&mut self, bytes: &[u8]) -> ProcessReport {
        let mut report = ProcessReport {
            commands_processed: 0,
            events: Vec::new(),
        };

        let mut cursor = Cursor::new(bytes);

        let header_ok = (|| -> Result<(), DecodeError> {
            let magic = cursor.read_u32_le()?;
            if magic != STREAM_MAGIC {
                return Err(cursor.error(format!(
                    "bad stream magic 0x{magic:08x} (expected 0x{STREAM_MAGIC:08x})"
                )));
            }
            let version_major = cursor.read_u16_le()?;
            let _version_minor = cursor.read_u16_le()?;
            if version_major != STREAM_VERSION_MAJOR {
                return Err(cursor.error(format!(
                    "unsupported stream major version {version_major} (expected {STREAM_VERSION_MAJOR})"
                )));
            }
            let payload_len = cursor.read_u32_le()? as usize;
            let expected_payload_len = bytes.len().saturating_sub(STREAM_HEADER_LEN);
            if payload_len != expected_payload_len {
                return Err(cursor.error(format!(
                    "stream payload length mismatch (header {payload_len}, actual {expected_payload_len})"
                )));
            }
            Ok(())
        })();

        if let Err(err) = header_ok {
            report.events.push(ProcessorEvent::Error {
                at: err.at,
                message: err.message,
            });
            return report;
        }

        while cursor.remaining() > 0 {
            let command_offset = cursor.offset;
            let header = (|| -> Result<(u16, u16, u32), DecodeError> {
                if cursor.remaining() < COMMAND_HEADER_LEN {
                    return Err(cursor.error("truncated command header".into()));
                }
                let opcode = cursor.read_u16_le()?;
                let flags = cursor.read_u16_le()?;
                let len = cursor.read_u32_le()?;
                Ok((opcode, flags, len))
            })();

            let (opcode_raw, flags, payload_len) = match header {
                Ok(v) => v,
                Err(err) => {
                    report.events.push(ProcessorEvent::Error {
                        at: err.at,
                        message: err.message,
                    });
                    break;
                }
            };

            if flags != 0 {
                report.events.push(ProcessorEvent::Error {
                    at: command_offset,
                    message: format!("command flags field must be 0, got {flags}"),
                });
                break;
            }

            let payload_len = payload_len as usize;
            if payload_len > cursor.remaining() {
                report.events.push(ProcessorEvent::Error {
                    at: command_offset,
                    message: format!(
                        "command payload length {payload_len} exceeds remaining bytes {}",
                        cursor.remaining()
                    ),
                });
                break;
            }

            let payload = match cursor.read_bytes(payload_len) {
                Ok(bytes) => bytes,
                Err(err) => {
                    report.events.push(ProcessorEvent::Error {
                        at: err.at,
                        message: err.message,
                    });
                    break;
                }
            };

            let Some(opcode) = Opcode::from_u16(opcode_raw) else {
                report.events.push(ProcessorEvent::Error {
                    at: command_offset,
                    message: format!("unknown opcode 0x{opcode_raw:04x}"),
                });
                break;
            };

            debug!(at = command_offset, opcode = %opcode, payload_len, "gpu command");

            let command_result = self
                .execute_command(opcode, command_offset, payload, &mut report.events)
                .await;

            report.commands_processed += 1;

            if let Err(err_msg) = command_result {
                report.events.push(ProcessorEvent::Error {
                    at: command_offset,
                    message: err_msg,
                });
                break;
            }
        }

        report
    }

    pub async fn readback_swapchain_rgba8(
        &self,
        device_id: u32,
        swapchain_id: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let device = self
            .devices
            .get(&device_id)
            .ok_or_else(|| format!("unknown device {device_id}"))?;
        device
            .runtime
            .readback_swapchain_rgba8(swapchain_id)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn readback_texture_rgba8(
        &self,
        device_id: u32,
        texture_handle: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let underlying = self
            .shared_textures
            .resolve_texture_handle(texture_handle)
            .ok_or_else(|| format!("unknown texture handle {texture_handle}"))?;

        let device = self
            .devices
            .get(&device_id)
            .ok_or_else(|| format!("unknown device {device_id}"))?;

        device
            .runtime
            .readback_texture_rgba8(underlying)
            .await
            .map_err(|e| e.to_string())
    }

    async fn execute_command(
        &mut self,
        opcode: Opcode,
        command_offset: usize,
        payload: &[u8],
        events: &mut Vec<ProcessorEvent>,
    ) -> Result<(), String> {
        let mut p = Cursor::new(payload);

        match opcode {
            Opcode::DeviceCreate => {
                let device_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("DeviceCreate payload must be exactly 4 bytes".into());
                }
                if self.devices.contains_key(&device_id) {
                    return Err(format!("device {device_id} already exists"));
                }

                let runtime = D3D9Runtime::new(RuntimeConfig {
                    validation: self.config.validation,
                })
                .await
                .map_err(|e| e.to_string())?;

                self.devices.insert(device_id, DeviceEntry { runtime });
                Ok(())
            }
            Opcode::DeviceDestroy => {
                let device_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("DeviceDestroy payload must be exactly 4 bytes".into());
                }
                if self.devices.remove(&device_id).is_none() {
                    return Err(format!("unknown device {device_id}"));
                }
                self.contexts.retain(|_, v| *v != device_id);
                // The test command processor uses a per-device runtime that owns all texture
                // resources. When a runtime is torn down, clear any outstanding alias/token state
                // to avoid dangling references.
                self.shared_textures.clear();
                Ok(())
            }
            Opcode::ContextCreate => {
                let device_id = p.read_u32_le().map_err(|e| e.message)?;
                let context_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("ContextCreate payload must be exactly 8 bytes".into());
                }
                if !self.devices.contains_key(&device_id) {
                    return Err(format!(
                        "ContextCreate references unknown device {device_id}"
                    ));
                }
                if self.contexts.contains_key(&context_id) {
                    return Err(format!("context {context_id} already exists"));
                }
                self.contexts.insert(context_id, device_id);
                Ok(())
            }
            Opcode::ContextDestroy => {
                let context_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("ContextDestroy payload must be exactly 4 bytes".into());
                }
                if self.contexts.remove(&context_id).is_none() {
                    return Err(format!("unknown context {context_id}"));
                }
                Ok(())
            }
            Opcode::ExportSharedSurface => {
                let resource_handle = p.read_u32_le().map_err(|e| e.message)?;
                p.read_u32_le().map_err(|e| e.message)?; // reserved0
                let share_token = p.read_u64_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("ExportSharedSurface payload must be exactly 16 bytes".into());
                }

                self.shared_textures
                    .export_shared_surface(resource_handle, share_token)
            }
            Opcode::ImportSharedSurface => {
                let out_resource_handle = p.read_u32_le().map_err(|e| e.message)?;
                p.read_u32_le().map_err(|e| e.message)?; // reserved0
                let share_token = p.read_u64_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("ImportSharedSurface payload must be exactly 16 bytes".into());
                }

                self.shared_textures
                    .import_shared_surface(out_resource_handle, share_token)
            }
            _ => {
                let context_id = p.read_u32_le().map_err(|e| e.message)?;
                let device_id = *self
                    .contexts
                    .get(&context_id)
                    .ok_or_else(|| format!("command references unknown context {context_id}"))?;
                let device = self.devices.get_mut(&device_id).ok_or_else(|| {
                    format!("context {context_id} references missing device {device_id}")
                })?;

                if self.config.validation {
                    device.runtime.begin_validation_scope();
                }

                let result = Self::execute_context_command(
                    &mut self.shared_textures,
                    &mut device.runtime,
                    opcode,
                    context_id,
                    &mut p,
                    events,
                )
                .await;

                if self.config.validation {
                    if let Some(error) = device.runtime.end_validation_scope().await {
                        warn!(at = command_offset, error = %error, "wgpu validation error");
                        return Err(format!("wgpu validation error: {error}"));
                    }
                }

                result
            }
        }
    }

    async fn execute_context_command(
        shared_textures: &mut SharedTextureState,
        runtime: &mut D3D9Runtime,
        opcode: Opcode,
        _context_id: u32,
        p: &mut Cursor<'_>,
        events: &mut Vec<ProcessorEvent>,
    ) -> Result<(), String> {
        match opcode {
            Opcode::SwapChainCreate => {
                let swapchain_id = p.read_u32_le().map_err(|e| e.message)?;
                let width = p.read_u32_le().map_err(|e| e.message)?;
                let height = p.read_u32_le().map_err(|e| e.message)?;
                let format_raw = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SwapChainCreate payload must be exactly 20 bytes".into());
                }
                let format = TextureFormat::from_u32(format_raw)
                    .ok_or_else(|| format!("unknown texture format {format_raw}"))?;
                let format = map_swapchain_format(format)?;

                runtime
                    .create_swap_chain(
                        swapchain_id,
                        SwapChainDesc {
                            width,
                            height,
                            format,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::SwapChainDestroy => {
                let swapchain_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SwapChainDestroy payload must be exactly 8 bytes".into());
                }
                runtime
                    .destroy_swap_chain(swapchain_id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::BufferCreate => {
                let buffer_id = p.read_u32_le().map_err(|e| e.message)?;
                let size = p.read_u64_le().map_err(|e| e.message)?;
                let usage = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("BufferCreate payload must be exactly 20 bytes".into());
                }

                runtime
                    .create_buffer(buffer_id, size, usage)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::BufferUpdate => {
                let buffer_id = p.read_u32_le().map_err(|e| e.message)?;
                let offset = p.read_u64_le().map_err(|e| e.message)?;
                let data = p.read_bytes(p.remaining()).map_err(|e| e.message)?;

                runtime
                    .write_buffer(buffer_id, offset, data)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::BufferDestroy => {
                let buffer_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("BufferDestroy payload must be exactly 8 bytes".into());
                }
                runtime
                    .destroy_buffer(buffer_id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::TextureCreate => {
                let texture_id = p.read_u32_le().map_err(|e| e.message)?;
                let width = p.read_u32_le().map_err(|e| e.message)?;
                let height = p.read_u32_le().map_err(|e| e.message)?;
                let mip_level_count = p.read_u32_le().map_err(|e| e.message)?;
                let format_raw = p.read_u32_le().map_err(|e| e.message)?;
                let usage = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("TextureCreate payload must be exactly 28 bytes".into());
                }

                if texture_id == 0 {
                    return Err("texture handle 0 is reserved".into());
                }
                if shared_textures.texture_handles.contains_key(&texture_id) {
                    return Err(format!("texture handle {texture_id} already exists"));
                }
                if shared_textures.texture_refcounts.contains_key(&texture_id) {
                    // Underlying handles remain reserved as long as any aliases still reference
                    // them. If the original handle was destroyed, reject reusing its numeric ID
                    // until the underlying texture is fully released.
                    return Err(format!(
                        "texture handle {texture_id} is still in use (underlying id kept alive by shared surface aliases)"
                    ));
                }

                let format = TextureFormat::from_u32(format_raw)
                    .ok_or_else(|| format!("unknown texture format {format_raw}"))?;

                runtime
                    .create_texture(
                        texture_id,
                        TextureDesc {
                            width,
                            height,
                            mip_level_count,
                            format: map_texture_format(format),
                            usage,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                shared_textures.register_texture_handle(texture_id, texture_id)?;
                Ok(())
            }
            Opcode::TextureUpdate => {
                let texture_handle = p.read_u32_le().map_err(|e| e.message)?;
                let mip_level = p.read_u32_le().map_err(|e| e.message)?;
                let width = p.read_u32_le().map_err(|e| e.message)?;
                let height = p.read_u32_le().map_err(|e| e.message)?;
                let data = p.read_bytes(p.remaining()).map_err(|e| e.message)?;

                let texture_id = shared_textures
                    .resolve_texture_handle(texture_handle)
                    .ok_or_else(|| format!("unknown texture handle {texture_handle}"))?;
                runtime
                    .write_texture_full_mip(texture_id, mip_level, width, height, data)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::TextureDestroy => {
                let texture_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("TextureDestroy payload must be exactly 8 bytes".into());
                }
                shared_textures.destroy_texture_handle(runtime, texture_id)?;
                Ok(())
            }
            Opcode::SetRenderTargets => match p.remaining() {
                4 => {
                    // Legacy encoding: context_id + swapchain_id
                    let swapchain_id = p.read_u32_le().map_err(|e| e.message)?;
                    runtime
                        .set_render_target_swapchain(swapchain_id)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                }
                16 => {
                    let color_kind = p.read_u32_le().map_err(|e| e.message)?;
                    let color_id = p.read_u32_le().map_err(|e| e.message)?;
                    let depth_kind = p.read_u32_le().map_err(|e| e.message)?;
                    let depth_id = p.read_u32_le().map_err(|e| e.message)?;
                    if p.remaining() != 0 {
                        return Err("SetRenderTargets payload must be 8 or 20 bytes".into());
                    }

                    let color = match color_kind {
                        0 => None,
                        1 => Some(RenderTarget::SwapChain(color_id)),
                        2 => {
                            let texture_id = shared_textures
                                .resolve_texture_handle(color_id)
                                .ok_or_else(|| format!("unknown texture handle {color_id}"))?;
                            Some(RenderTarget::Texture(texture_id))
                        }
                        _ => return Err(format!("unknown color render target kind {color_kind}")),
                    };

                    let depth = match depth_kind {
                        0 => None,
                        2 => Some(
                            shared_textures
                                .resolve_texture_handle(depth_id)
                                .ok_or_else(|| format!("unknown texture handle {depth_id}"))?,
                        ),
                        _ => return Err(format!("unknown depth-stencil target kind {depth_kind}")),
                    };

                    runtime
                        .set_render_targets(color, depth)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                }
                other => Err(format!("SetRenderTargets payload has invalid size {other}")),
            },
            Opcode::SetShaderKey => {
                let stage_raw = p.read_u8().map_err(|e| e.message)?;
                p.skip(3).map_err(|e| e.message)?;
                let shader_key = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SetShaderKey payload must be exactly 12 bytes".into());
                }

                let stage = ShaderStage::from_u8(stage_raw)
                    .ok_or_else(|| format!("unknown shader stage {stage_raw}"))?;

                runtime
                    .set_shader_key(map_shader_stage(stage), shader_key)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::SetConstantsF32 => {
                let stage_raw = p.read_u8().map_err(|e| e.message)?;
                p.read_u8().map_err(|e| e.message)?; // reserved
                let start_register = p.read_u16_le().map_err(|e| e.message)?;
                let vec4_count = p.read_u16_le().map_err(|e| e.message)?;
                p.read_u16_le().map_err(|e| e.message)?; // reserved

                let float_count = vec4_count as usize * 4;
                if p.remaining() != float_count * 4 {
                    return Err(format!(
                        "SetConstantsF32 payload length mismatch (expected {} bytes of data, got {})",
                        float_count * 4,
                        p.remaining()
                    ));
                }

                let mut floats = Vec::new();
                floats.try_reserve_exact(float_count).map_err(|_| {
                    format!("SetConstantsF32 payload too large to allocate (float_count={float_count})")
                })?;
                for _ in 0..float_count {
                    floats.push(f32::from_bits(p.read_u32_le().map_err(|e| e.message)?));
                }

                let stage = ShaderStage::from_u8(stage_raw)
                    .ok_or_else(|| format!("unknown shader stage {stage_raw}"))?;
                runtime
                    .set_constants_f32(map_shader_stage(stage), start_register, &floats)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::SetRenderStateU32 => {
                let state_id = p.read_u32_le().map_err(|e| e.message)?;
                let value = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SetRenderStateU32 payload must be exactly 12 bytes".into());
                }
                runtime.set_render_state_u32(state_id, value);
                Ok(())
            }
            Opcode::SetVertexDeclaration => {
                let stride = p.read_u32_le().map_err(|e| e.message)?;
                let attr_count_u32 = p.read_u32_le().map_err(|e| e.message)?;
                let attr_count = usize::try_from(attr_count_u32).map_err(|_| {
                    format!("SetVertexDeclaration attr_count={attr_count_u32} is out of range for usize")
                })?;
                let expected_bytes = attr_count.checked_mul(12).ok_or_else(|| {
                    format!("SetVertexDeclaration attr_count={attr_count_u32} is too large")
                })?;
                if p.remaining() != expected_bytes {
                    return Err(format!(
                        "SetVertexDeclaration payload size mismatch (attr_count {attr_count_u32}, expected {expected_bytes} bytes, got {})",
                        p.remaining()
                    ));
                }

                let mut attributes = Vec::new();
                attributes.try_reserve_exact(attr_count).map_err(|_| {
                    format!("SetVertexDeclaration payload too large to allocate (attr_count={attr_count_u32})")
                })?;
                for _ in 0..attr_count {
                    let location = p.read_u32_le().map_err(|e| e.message)?;
                    let format_raw = p.read_u32_le().map_err(|e| e.message)?;
                    let offset = p.read_u32_le().map_err(|e| e.message)?;
                    let format = VertexFormat::from_u32(format_raw)
                        .ok_or_else(|| format!("unknown vertex format {format_raw}"))?;
                    attributes.push(VertexAttributeDesc {
                        location,
                        format: map_vertex_format(format),
                        offset,
                    });
                }

                runtime
                    .set_vertex_decl(VertexDecl {
                        stride: stride as u64,
                        attributes,
                    })
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::SetVertexStream => {
                let stream_index = p.read_u8().map_err(|e| e.message)?;
                p.skip(3).map_err(|e| e.message)?;
                let buffer_id = p.read_u32_le().map_err(|e| e.message)?;
                let offset = p.read_u64_le().map_err(|e| e.message)?;
                let stride = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SetVertexStream payload must be exactly 24 bytes".into());
                }
                if stream_index != 0 {
                    return Err(format!(
                        "only stream 0 is supported (got stream {stream_index})"
                    ));
                }
                runtime
                    .set_vertex_stream0(buffer_id, offset, stride as u64)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::SetIndexBuffer => {
                let buffer_id = p.read_u32_le().map_err(|e| e.message)?;
                let offset = p.read_u64_le().map_err(|e| e.message)?;
                let format_raw = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SetIndexBuffer payload must be exactly 20 bytes".into());
                }
                let format = IndexFormat::from_u32(format_raw)
                    .ok_or_else(|| format!("unknown index format {format_raw}"))?;
                runtime
                    .set_index_buffer(buffer_id, offset, map_index_format(format))
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::SetSamplerStateU32 => {
                let stage_raw = p.read_u8().map_err(|e| e.message)?;
                let slot = p.read_u8().map_err(|e| e.message)?;
                p.skip(2).map_err(|e| e.message)?;
                let state_id = p.read_u32_le().map_err(|e| e.message)?;
                let value = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SetSamplerStateU32 payload must be exactly 16 bytes".into());
                }

                let stage = ShaderStage::from_u8(stage_raw)
                    .ok_or_else(|| format!("unknown shader stage {stage_raw}"))?;
                runtime.set_sampler_state_u32(
                    map_shader_stage(stage),
                    slot as u32,
                    state_id,
                    value,
                );
                Ok(())
            }
            Opcode::SetTexture => {
                let stage_raw = p.read_u8().map_err(|e| e.message)?;
                let slot = p.read_u8().map_err(|e| e.message)?;
                p.skip(2).map_err(|e| e.message)?;
                let texture_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SetTexture payload must be exactly 12 bytes".into());
                }

                let stage = ShaderStage::from_u8(stage_raw)
                    .ok_or_else(|| format!("unknown shader stage {stage_raw}"))?;
                let texture = if texture_id == 0 {
                    None
                } else {
                    Some(texture_id)
                };
                runtime
                    .set_texture(map_shader_stage(stage), slot as u32, texture)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::SetViewport => {
                let x = f32::from_bits(p.read_u32_le().map_err(|e| e.message)?);
                let y = f32::from_bits(p.read_u32_le().map_err(|e| e.message)?);
                let width = f32::from_bits(p.read_u32_le().map_err(|e| e.message)?);
                let height = f32::from_bits(p.read_u32_le().map_err(|e| e.message)?);
                let min_depth = f32::from_bits(p.read_u32_le().map_err(|e| e.message)?);
                let max_depth = f32::from_bits(p.read_u32_le().map_err(|e| e.message)?);
                if p.remaining() != 0 {
                    return Err("SetViewport payload must be exactly 28 bytes".into());
                }

                runtime.set_viewport(Viewport {
                    x,
                    y,
                    width,
                    height,
                    min_depth,
                    max_depth,
                });
                Ok(())
            }
            Opcode::SetScissorRect => {
                let x = p.read_u32_le().map_err(|e| e.message)?;
                let y = p.read_u32_le().map_err(|e| e.message)?;
                let width = p.read_u32_le().map_err(|e| e.message)?;
                let height = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("SetScissorRect payload must be exactly 20 bytes".into());
                }

                runtime.set_scissor_rect(ScissorRect {
                    x,
                    y,
                    width,
                    height,
                });
                Ok(())
            }
            Opcode::Draw => {
                let vertex_count = p.read_u32_le().map_err(|e| e.message)?;
                let first_vertex = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("Draw payload must be exactly 12 bytes".into());
                }
                runtime
                    .draw(vertex_count, first_vertex)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::DrawIndexed => {
                let index_count = p.read_u32_le().map_err(|e| e.message)?;
                let first_index = p.read_u32_le().map_err(|e| e.message)?;
                let base_vertex = p.read_i32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("DrawIndexed payload must be exactly 16 bytes".into());
                }

                runtime
                    .draw_indexed(index_count, first_index, base_vertex)
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::Present => {
                let swapchain_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("Present payload must be exactly 8 bytes".into());
                }

                // Ensure it exists (and that errors are caught early).
                runtime
                    .set_render_target_swapchain(swapchain_id)
                    .map_err(|e| e.to_string())?;
                runtime.present().map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::FenceCreate => {
                let fence_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("FenceCreate payload must be exactly 8 bytes".into());
                }
                runtime.fence_create(fence_id);
                Ok(())
            }
            Opcode::FenceSignal => {
                let fence_id = p.read_u32_le().map_err(|e| e.message)?;
                let value = p.read_u64_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("FenceSignal payload must be exactly 16 bytes".into());
                }
                runtime
                    .fence_signal(fence_id, value)
                    .await
                    .map_err(|e| e.to_string())?;
                events.push(ProcessorEvent::FenceSignaled { fence_id, value });
                Ok(())
            }
            Opcode::FenceWait => {
                let fence_id = p.read_u32_le().map_err(|e| e.message)?;
                let value = p.read_u64_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("FenceWait payload must be exactly 16 bytes".into());
                }
                runtime
                    .fence_wait(fence_id, value)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            Opcode::FenceDestroy => {
                let fence_id = p.read_u32_le().map_err(|e| e.message)?;
                if p.remaining() != 0 {
                    return Err("FenceDestroy payload must be exactly 8 bytes".into());
                }
                runtime.fence_destroy(fence_id);
                Ok(())
            }
            Opcode::DeviceCreate
            | Opcode::DeviceDestroy
            | Opcode::ContextCreate
            | Opcode::ContextDestroy
            | Opcode::ExportSharedSurface
            | Opcode::ImportSharedSurface => {
                Err(format!("opcode {opcode:?} dispatched incorrectly"))
            }
        }
    }
}

#[derive(Debug)]
struct DecodeError {
    at: usize,
    message: String,
}

#[derive(Debug)]
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn error(&self, message: String) -> DecodeError {
        DecodeError {
            at: self.offset,
            message,
        }
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < len {
            return Err(self.error(format!(
                "truncated read (need {len} bytes, have {})",
                self.remaining()
            )));
        }
        let start = self.offset;
        let end = start + len;
        self.offset = end;
        Ok(&self.bytes[start..end])
    }

    fn skip(&mut self, len: usize) -> Result<(), DecodeError> {
        self.read_bytes(len).map(|_| ())
    }

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_u16_le(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32_le(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64_le(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_i32_le(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.read_u32_le()?.to_le_bytes()))
    }
}

fn map_swapchain_format(format: TextureFormat) -> Result<ColorFormat, String> {
    match format {
        TextureFormat::Rgba8Unorm => Ok(ColorFormat::Rgba8Unorm),
        TextureFormat::Rgba8UnormSrgb => Ok(ColorFormat::Rgba8UnormSrgb),
        TextureFormat::Depth24PlusStencil8 => {
            Err("swapchains cannot use depth-stencil formats".into())
        }
    }
}

fn map_texture_format(format: TextureFormat) -> RuntimeTextureFormat {
    match format {
        TextureFormat::Rgba8Unorm => RuntimeTextureFormat::Color(ColorFormat::Rgba8Unorm),
        TextureFormat::Rgba8UnormSrgb => RuntimeTextureFormat::Color(ColorFormat::Rgba8UnormSrgb),
        TextureFormat::Depth24PlusStencil8 => RuntimeTextureFormat::Depth24PlusStencil8,
    }
}

fn map_index_format(format: IndexFormat) -> RuntimeIndexFormat {
    match format {
        IndexFormat::U16 => RuntimeIndexFormat::U16,
        IndexFormat::U32 => RuntimeIndexFormat::U32,
    }
}

fn map_shader_stage(stage: ShaderStage) -> RuntimeShaderStage {
    match stage {
        ShaderStage::Vertex => RuntimeShaderStage::Vertex,
        ShaderStage::Fragment => RuntimeShaderStage::Fragment,
    }
}

fn map_vertex_format(format: VertexFormat) -> RuntimeVertexFormat {
    match format {
        VertexFormat::Float32x2 => RuntimeVertexFormat::Float32x2,
        VertexFormat::Float32x3 => RuntimeVertexFormat::Float32x3,
        VertexFormat::Float32x4 => RuntimeVertexFormat::Float32x4,
        VertexFormat::Unorm8x4 => RuntimeVertexFormat::Unorm8x4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol_d3d9::{STREAM_MAGIC, STREAM_VERSION_MAJOR, STREAM_VERSION_MINOR};

    #[test]
    fn rejects_unknown_opcode() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&STREAM_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&STREAM_VERSION_MAJOR.to_le_bytes());
        bytes.extend_from_slice(&STREAM_VERSION_MINOR.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // payload len patched later

        // Unknown opcode, zero-length payload.
        bytes.extend_from_slice(&0xFFFFu16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        let payload_len = (bytes.len() - STREAM_HEADER_LEN) as u32;
        bytes[8..12].copy_from_slice(&payload_len.to_le_bytes());

        let mut processor = CommandProcessor::new(ProcessorConfig::default());
        let report = pollster::block_on(processor.process(&bytes));
        assert!(
            report
                .events
                .iter()
                .any(|e| matches!(e, ProcessorEvent::Error { .. })),
            "expected an error event, got: {:?}",
            report.events
        );
    }

    #[test]
    fn rejects_truncated_command_payload() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&STREAM_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&STREAM_VERSION_MAJOR.to_le_bytes());
        bytes.extend_from_slice(&STREAM_VERSION_MINOR.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // payload len patched later

        // Command header claims 4 bytes but provides none.
        bytes.extend_from_slice(&(Opcode::DeviceCreate as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());

        let payload_len = (bytes.len() - STREAM_HEADER_LEN) as u32;
        bytes[8..12].copy_from_slice(&payload_len.to_le_bytes());

        let mut processor = CommandProcessor::new(ProcessorConfig::default());
        let report = pollster::block_on(processor.process(&bytes));
        assert!(
            report
                .events
                .iter()
                .any(|e| matches!(e, ProcessorEvent::Error { .. })),
            "expected an error event, got: {:?}",
            report.events
        );
    }
}
