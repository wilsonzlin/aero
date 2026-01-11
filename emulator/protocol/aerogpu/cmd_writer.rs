//! Safe command stream builder for AeroGPU (`aerogpu_cmd.h`).
//!
//! This is intended for tests/fixtures and host-side tooling that needs to emit
//! canonical command streams (correct packet `size_bytes`, padding/alignment,
//! and stream header bookkeeping).

use core::mem::{offset_of, size_of};

use super::aerogpu_cmd::{
    AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdClear, AerogpuCmdCopyBuffer, AerogpuCmdCopyTexture2d,
    AerogpuCmdCreateBuffer, AerogpuCmdCreateInputLayout, AerogpuCmdCreateShaderDxbc, AerogpuCmdCreateTexture2d,
    AerogpuCmdDestroyInputLayout, AerogpuCmdDestroyResource, AerogpuCmdDestroyShader, AerogpuCmdDraw,
    AerogpuCmdDrawIndexed, AerogpuCmdExportSharedSurface, AerogpuCmdFlush, AerogpuCmdImportSharedSurface,
    AerogpuCmdOpcode, AerogpuCmdPresent, AerogpuCmdPresentEx, AerogpuCmdResourceDirtyRange, AerogpuCmdSetBlendState,
    AerogpuCmdSetDepthStencilState, AerogpuCmdSetIndexBuffer, AerogpuCmdSetInputLayout,
    AerogpuCmdSetPrimitiveTopology, AerogpuCmdSetRasterizerState, AerogpuCmdSetRenderState, AerogpuCmdSetRenderTargets,
    AerogpuCmdSetSamplerState, AerogpuCmdSetScissor, AerogpuCmdSetShaderConstantsF, AerogpuCmdSetTexture,
    AerogpuCmdSetVertexBuffers, AerogpuCmdSetViewport, AerogpuCmdStreamFlags, AerogpuCmdStreamHeader,
    AerogpuCmdUploadResource, AerogpuCompareFunc, AerogpuCullMode, AerogpuFillMode, AerogpuHandle, AerogpuIndexFormat,
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_MAX_RENDER_TARGETS,
};
use super::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

fn align_up(v: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    (v + (a - 1)) & !(a - 1)
}

/// Safe command stream builder for `aerogpu_cmd.h`.
#[derive(Debug, Default, Clone)]
pub struct AerogpuCmdWriter {
    buf: Vec<u8>,
}

impl AerogpuCmdWriter {
    pub fn new() -> Self {
        let mut w = Self { buf: Vec::new() };
        w.reset();
        w
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.buf.resize(AerogpuCmdStreamHeader::SIZE_BYTES, 0);

        self.write_u32_at(0, AEROGPU_CMD_STREAM_MAGIC);
        self.write_u32_at(4, AEROGPU_ABI_VERSION_U32);
        self.write_u32_at(8, AerogpuCmdStreamHeader::SIZE_BYTES as u32);
        self.write_u32_at(12, AerogpuCmdStreamFlags::None as u32);
    }

    pub fn finish(mut self) -> Vec<u8> {
        assert!(
            self.buf.len() <= u32::MAX as usize,
            "command stream too large for u32 size_bytes"
        );
        self.write_u32_at(8, self.buf.len() as u32);
        self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.len() <= AerogpuCmdStreamHeader::SIZE_BYTES
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    fn write_u32_at(&mut self, offset: usize, v: u32) {
        self.buf[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn write_i32_at(&mut self, offset: usize, v: i32) {
        self.write_u32_at(offset, v as u32);
    }

    fn write_u8_at(&mut self, offset: usize, v: u8) {
        self.buf[offset] = v;
    }

    fn write_u64_at(&mut self, offset: usize, v: u64) {
        self.buf[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn append_raw(&mut self, opcode: AerogpuCmdOpcode, cmd_size_bytes: usize) -> usize {
        let aligned_size = align_up(cmd_size_bytes, 4);
        assert!(
            aligned_size <= u32::MAX as usize,
            "command packet too large for u32 size_bytes"
        );

        let offset = self.buf.len();
        self.buf.resize(offset + aligned_size, 0);

        self.write_u32_at(offset, opcode as u32);
        self.write_u32_at(offset + 4, aligned_size as u32);
        offset
    }

    pub fn create_buffer(
        &mut self,
        buffer_handle: AerogpuHandle,
        usage_flags: u32,
        size_bytes: u64,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
    ) {
        let base = self.append_raw(AerogpuCmdOpcode::CreateBuffer, size_of::<AerogpuCmdCreateBuffer>());
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateBuffer, buffer_handle), buffer_handle);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateBuffer, usage_flags), usage_flags);
        self.write_u64_at(base + offset_of!(AerogpuCmdCreateBuffer, size_bytes), size_bytes);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateBuffer, backing_alloc_id), backing_alloc_id);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateBuffer, backing_offset_bytes),
            backing_offset_bytes,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_texture2d(
        &mut self,
        texture_handle: AerogpuHandle,
        usage_flags: u32,
        format: u32,
        width: u32,
        height: u32,
        mip_levels: u32,
        array_layers: u32,
        row_pitch_bytes: u32,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::CreateTexture2d,
            size_of::<AerogpuCmdCreateTexture2d>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, texture_handle), texture_handle);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, usage_flags), usage_flags);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, format), format);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, width), width);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, height), height);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, mip_levels), mip_levels);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, array_layers), array_layers);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, row_pitch_bytes), row_pitch_bytes);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, backing_alloc_id), backing_alloc_id);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateTexture2d, backing_offset_bytes),
            backing_offset_bytes,
        );
    }

    pub fn destroy_resource(&mut self, resource_handle: AerogpuHandle) {
        let base = self.append_raw(
            AerogpuCmdOpcode::DestroyResource,
            size_of::<AerogpuCmdDestroyResource>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdDestroyResource, resource_handle), resource_handle);
    }

    pub fn resource_dirty_range(&mut self, resource_handle: AerogpuHandle, offset_bytes: u64, size_bytes: u64) {
        let base = self.append_raw(
            AerogpuCmdOpcode::ResourceDirtyRange,
            size_of::<AerogpuCmdResourceDirtyRange>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdResourceDirtyRange, resource_handle),
            resource_handle,
        );
        self.write_u64_at(base + offset_of!(AerogpuCmdResourceDirtyRange, offset_bytes), offset_bytes);
        self.write_u64_at(base + offset_of!(AerogpuCmdResourceDirtyRange, size_bytes), size_bytes);
    }

    pub fn upload_resource(&mut self, resource_handle: AerogpuHandle, offset_bytes: u64, data: &[u8]) {
        assert!(data.len() <= u64::MAX as usize);
        let unpadded_size = size_of::<AerogpuCmdUploadResource>() + data.len();
        let base = self.append_raw(AerogpuCmdOpcode::UploadResource, unpadded_size);
        self.write_u32_at(base + offset_of!(AerogpuCmdUploadResource, resource_handle), resource_handle);
        self.write_u64_at(base + offset_of!(AerogpuCmdUploadResource, offset_bytes), offset_bytes);
        self.write_u64_at(base + offset_of!(AerogpuCmdUploadResource, size_bytes), data.len() as u64);
        self.buf[base + size_of::<AerogpuCmdUploadResource>()..base + size_of::<AerogpuCmdUploadResource>() + data.len()]
            .copy_from_slice(data);
    }

    pub fn copy_buffer(
        &mut self,
        dst_buffer: AerogpuHandle,
        src_buffer: AerogpuHandle,
        dst_offset_bytes: u64,
        src_offset_bytes: u64,
        size_bytes: u64,
        flags: u32,
    ) {
        let base = self.append_raw(AerogpuCmdOpcode::CopyBuffer, size_of::<AerogpuCmdCopyBuffer>());
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyBuffer, dst_buffer), dst_buffer);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyBuffer, src_buffer), src_buffer);
        self.write_u64_at(base + offset_of!(AerogpuCmdCopyBuffer, dst_offset_bytes), dst_offset_bytes);
        self.write_u64_at(base + offset_of!(AerogpuCmdCopyBuffer, src_offset_bytes), src_offset_bytes);
        self.write_u64_at(base + offset_of!(AerogpuCmdCopyBuffer, size_bytes), size_bytes);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyBuffer, flags), flags);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn copy_texture2d(
        &mut self,
        dst_texture: AerogpuHandle,
        src_texture: AerogpuHandle,
        dst_mip_level: u32,
        dst_array_layer: u32,
        src_mip_level: u32,
        src_array_layer: u32,
        dst_x: u32,
        dst_y: u32,
        src_x: u32,
        src_y: u32,
        width: u32,
        height: u32,
        flags: u32,
    ) {
        let base = self.append_raw(AerogpuCmdOpcode::CopyTexture2d, size_of::<AerogpuCmdCopyTexture2d>());
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, dst_texture), dst_texture);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, src_texture), src_texture);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, dst_mip_level), dst_mip_level);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, dst_array_layer), dst_array_layer);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, src_mip_level), src_mip_level);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, src_array_layer), src_array_layer);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, dst_x), dst_x);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, dst_y), dst_y);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, src_x), src_x);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, src_y), src_y);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, width), width);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, height), height);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, flags), flags);
    }

    pub fn create_shader_dxbc(
        &mut self,
        shader_handle: AerogpuHandle,
        stage: AerogpuShaderStage,
        dxbc_bytes: &[u8],
    ) {
        assert!(dxbc_bytes.len() <= u32::MAX as usize);
        let unpadded_size = size_of::<AerogpuCmdCreateShaderDxbc>() + dxbc_bytes.len();
        let base = self.append_raw(AerogpuCmdOpcode::CreateShaderDxbc, unpadded_size);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateShaderDxbc, shader_handle), shader_handle);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateShaderDxbc, stage), stage as u32);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes),
            dxbc_bytes.len() as u32,
        );
        self.buf[base + size_of::<AerogpuCmdCreateShaderDxbc>()..base + size_of::<AerogpuCmdCreateShaderDxbc>() + dxbc_bytes.len()]
            .copy_from_slice(dxbc_bytes);
    }

    pub fn destroy_shader(&mut self, shader_handle: AerogpuHandle) {
        let base = self.append_raw(AerogpuCmdOpcode::DestroyShader, size_of::<AerogpuCmdDestroyShader>());
        self.write_u32_at(base + offset_of!(AerogpuCmdDestroyShader, shader_handle), shader_handle);
    }

    pub fn bind_shaders(&mut self, vs: AerogpuHandle, ps: AerogpuHandle, cs: AerogpuHandle) {
        use super::aerogpu_cmd::AerogpuCmdBindShaders;

        let base = self.append_raw(AerogpuCmdOpcode::BindShaders, size_of::<AerogpuCmdBindShaders>());
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, vs), vs);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, ps), ps);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, cs), cs);
    }

    pub fn create_input_layout(&mut self, input_layout_handle: AerogpuHandle, blob: &[u8]) {
        assert!(blob.len() <= u32::MAX as usize);
        let unpadded_size = size_of::<AerogpuCmdCreateInputLayout>() + blob.len();
        let base = self.append_raw(AerogpuCmdOpcode::CreateInputLayout, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateInputLayout, input_layout_handle),
            input_layout_handle,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateInputLayout, blob_size_bytes),
            blob.len() as u32,
        );
        self.buf[base + size_of::<AerogpuCmdCreateInputLayout>()..base + size_of::<AerogpuCmdCreateInputLayout>() + blob.len()]
            .copy_from_slice(blob);
    }

    pub fn destroy_input_layout(&mut self, input_layout_handle: AerogpuHandle) {
        let base = self.append_raw(
            AerogpuCmdOpcode::DestroyInputLayout,
            size_of::<AerogpuCmdDestroyInputLayout>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDestroyInputLayout, input_layout_handle),
            input_layout_handle,
        );
    }

    pub fn set_input_layout(&mut self, input_layout_handle: AerogpuHandle) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetInputLayout,
            size_of::<AerogpuCmdSetInputLayout>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetInputLayout, input_layout_handle), input_layout_handle);
    }

    pub fn set_render_targets(&mut self, colors: &[AerogpuHandle], depth_stencil: AerogpuHandle) {
        assert!(
            colors.len() <= AEROGPU_MAX_RENDER_TARGETS,
            "too many render targets ({} > {AEROGPU_MAX_RENDER_TARGETS})",
            colors.len()
        );
        let base = self.append_raw(
            AerogpuCmdOpcode::SetRenderTargets,
            size_of::<AerogpuCmdSetRenderTargets>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetRenderTargets, color_count), colors.len() as u32);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetRenderTargets, depth_stencil), depth_stencil);

        let colors_base = base + offset_of!(AerogpuCmdSetRenderTargets, colors);
        for (i, &h) in colors.iter().enumerate() {
            self.write_u32_at(colors_base + i * size_of::<AerogpuHandle>(), h);
        }
    }

    pub fn set_viewport(&mut self, x: f32, y: f32, width: f32, height: f32, min_depth: f32, max_depth: f32) {
        let base = self.append_raw(AerogpuCmdOpcode::SetViewport, size_of::<AerogpuCmdSetViewport>());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetViewport, x_f32), x.to_bits());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetViewport, y_f32), y.to_bits());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetViewport, width_f32), width.to_bits());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetViewport, height_f32), height.to_bits());
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetViewport, min_depth_f32),
            min_depth.to_bits(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetViewport, max_depth_f32),
            max_depth.to_bits(),
        );
    }

    pub fn set_scissor(&mut self, x: i32, y: i32, width: i32, height: i32) {
        let base = self.append_raw(AerogpuCmdOpcode::SetScissor, size_of::<AerogpuCmdSetScissor>());
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, x), x);
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, y), y);
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, width), width);
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, height), height);
    }

    pub fn set_vertex_buffers(&mut self, start_slot: u32, bindings: &[AerogpuVertexBufferBinding]) {
        assert!(bindings.len() <= u32::MAX as usize);
        let unpadded_size = size_of::<AerogpuCmdSetVertexBuffers>() + core::mem::size_of_val(bindings);
        let base = self.append_raw(AerogpuCmdOpcode::SetVertexBuffers, unpadded_size);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetVertexBuffers, start_slot), start_slot);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetVertexBuffers, buffer_count),
            bindings.len() as u32,
        );

        let bindings_base = base + size_of::<AerogpuCmdSetVertexBuffers>();
        for (i, binding) in bindings.iter().enumerate() {
            let b = bindings_base + i * size_of::<AerogpuVertexBufferBinding>();
            self.write_u32_at(b + offset_of!(AerogpuVertexBufferBinding, buffer), binding.buffer);
            self.write_u32_at(b + offset_of!(AerogpuVertexBufferBinding, stride_bytes), binding.stride_bytes);
            self.write_u32_at(b + offset_of!(AerogpuVertexBufferBinding, offset_bytes), binding.offset_bytes);
        }
    }

    pub fn set_index_buffer(&mut self, buffer: AerogpuHandle, format: AerogpuIndexFormat, offset_bytes: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::SetIndexBuffer, size_of::<AerogpuCmdSetIndexBuffer>());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetIndexBuffer, buffer), buffer);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetIndexBuffer, format), format as u32);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetIndexBuffer, offset_bytes), offset_bytes);
    }

    pub fn set_primitive_topology(&mut self, topology: AerogpuPrimitiveTopology) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetPrimitiveTopology,
            size_of::<AerogpuCmdSetPrimitiveTopology>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetPrimitiveTopology, topology), topology as u32);
    }

    pub fn set_texture(&mut self, shader_stage: AerogpuShaderStage, slot: u32, texture: AerogpuHandle) {
        let base = self.append_raw(AerogpuCmdOpcode::SetTexture, size_of::<AerogpuCmdSetTexture>());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, shader_stage), shader_stage as u32);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, slot), slot);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, texture), texture);
    }

    pub fn set_sampler_state(&mut self, shader_stage: AerogpuShaderStage, slot: u32, state: u32, value: u32) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetSamplerState,
            size_of::<AerogpuCmdSetSamplerState>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplerState, shader_stage), shader_stage as u32);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplerState, slot), slot);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplerState, state), state);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplerState, value), value);
    }

    pub fn set_render_state(&mut self, state: u32, value: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::SetRenderState, size_of::<AerogpuCmdSetRenderState>());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetRenderState, state), state);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetRenderState, value), value);
    }

    pub fn set_shader_constants_f(
        &mut self,
        stage: AerogpuShaderStage,
        start_register: u32,
        data: &[f32],
    ) {
        assert_eq!(
            data.len() % 4,
            0,
            "SET_SHADER_CONSTANTS_F data must be float4-aligned (got {} floats)",
            data.len()
        );
        assert!(data.len() <= u32::MAX as usize);

        let vec4_count = (data.len() / 4) as u32;
        let unpadded_size = size_of::<AerogpuCmdSetShaderConstantsF>() + data.len() * 4;
        let base = self.append_raw(AerogpuCmdOpcode::SetShaderConstantsF, unpadded_size);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetShaderConstantsF, stage), stage as u32);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, start_register),
            start_register,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetShaderConstantsF, vec4_count), vec4_count);

        let payload_base = base + size_of::<AerogpuCmdSetShaderConstantsF>();
        for (i, &v) in data.iter().enumerate() {
            self.write_u32_at(payload_base + i * 4, v.to_bits());
        }
    }

    pub fn set_blend_state(
        &mut self,
        enable: bool,
        src_factor: AerogpuBlendFactor,
        dst_factor: AerogpuBlendFactor,
        blend_op: AerogpuBlendOp,
        color_write_mask: u8,
    ) {
        use super::aerogpu_cmd::AerogpuBlendState;

        let base = self.append_raw(AerogpuCmdOpcode::SetBlendState, size_of::<AerogpuCmdSetBlendState>());
        let state_base = base + offset_of!(AerogpuCmdSetBlendState, state);
        self.write_u32_at(state_base + offset_of!(AerogpuBlendState, enable), enable as u32);
        self.write_u32_at(state_base + offset_of!(AerogpuBlendState, src_factor), src_factor as u32);
        self.write_u32_at(state_base + offset_of!(AerogpuBlendState, dst_factor), dst_factor as u32);
        self.write_u32_at(state_base + offset_of!(AerogpuBlendState, blend_op), blend_op as u32);
        self.write_u8_at(state_base + offset_of!(AerogpuBlendState, color_write_mask), color_write_mask);
    }

    pub fn set_depth_stencil_state(
        &mut self,
        depth_enable: bool,
        depth_write_enable: bool,
        depth_func: AerogpuCompareFunc,
        stencil_enable: bool,
        stencil_read_mask: u8,
        stencil_write_mask: u8,
    ) {
        use super::aerogpu_cmd::AerogpuDepthStencilState;

        let base = self.append_raw(
            AerogpuCmdOpcode::SetDepthStencilState,
            size_of::<AerogpuCmdSetDepthStencilState>(),
        );
        let state_base = base + offset_of!(AerogpuCmdSetDepthStencilState, state);
        self.write_u32_at(
            state_base + offset_of!(AerogpuDepthStencilState, depth_enable),
            depth_enable as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuDepthStencilState, depth_write_enable),
            depth_write_enable as u32,
        );
        self.write_u32_at(state_base + offset_of!(AerogpuDepthStencilState, depth_func), depth_func as u32);
        self.write_u32_at(
            state_base + offset_of!(AerogpuDepthStencilState, stencil_enable),
            stencil_enable as u32,
        );
        self.write_u8_at(
            state_base + offset_of!(AerogpuDepthStencilState, stencil_read_mask),
            stencil_read_mask,
        );
        self.write_u8_at(
            state_base + offset_of!(AerogpuDepthStencilState, stencil_write_mask),
            stencil_write_mask,
        );
    }

    pub fn set_rasterizer_state(
        &mut self,
        fill_mode: AerogpuFillMode,
        cull_mode: AerogpuCullMode,
        front_ccw: bool,
        scissor_enable: bool,
        depth_bias: i32,
    ) {
        use super::aerogpu_cmd::AerogpuRasterizerState;

        let base = self.append_raw(
            AerogpuCmdOpcode::SetRasterizerState,
            size_of::<AerogpuCmdSetRasterizerState>(),
        );
        let state_base = base + offset_of!(AerogpuCmdSetRasterizerState, state);
        self.write_u32_at(state_base + offset_of!(AerogpuRasterizerState, fill_mode), fill_mode as u32);
        self.write_u32_at(state_base + offset_of!(AerogpuRasterizerState, cull_mode), cull_mode as u32);
        self.write_u32_at(
            state_base + offset_of!(AerogpuRasterizerState, front_ccw),
            front_ccw as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuRasterizerState, scissor_enable),
            scissor_enable as u32,
        );
        self.write_i32_at(state_base + offset_of!(AerogpuRasterizerState, depth_bias), depth_bias);
    }

    pub fn clear(&mut self, flags: u32, color_rgba: [f32; 4], depth: f32, stencil: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::Clear, size_of::<AerogpuCmdClear>());
        self.write_u32_at(base + offset_of!(AerogpuCmdClear, flags), flags);

        let color_base = base + offset_of!(AerogpuCmdClear, color_rgba_f32);
        for (i, c) in color_rgba.iter().enumerate() {
            self.write_u32_at(color_base + i * 4, c.to_bits());
        }

        self.write_u32_at(base + offset_of!(AerogpuCmdClear, depth_f32), depth.to_bits());
        self.write_u32_at(base + offset_of!(AerogpuCmdClear, stencil), stencil);
    }

    pub fn draw(&mut self, vertex_count: u32, instance_count: u32, first_vertex: u32, first_instance: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::Draw, size_of::<AerogpuCmdDraw>());
        self.write_u32_at(base + offset_of!(AerogpuCmdDraw, vertex_count), vertex_count);
        self.write_u32_at(base + offset_of!(AerogpuCmdDraw, instance_count), instance_count);
        self.write_u32_at(base + offset_of!(AerogpuCmdDraw, first_vertex), first_vertex);
        self.write_u32_at(base + offset_of!(AerogpuCmdDraw, first_instance), first_instance);
    }

    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    ) {
        let base = self.append_raw(AerogpuCmdOpcode::DrawIndexed, size_of::<AerogpuCmdDrawIndexed>());
        self.write_u32_at(base + offset_of!(AerogpuCmdDrawIndexed, index_count), index_count);
        self.write_u32_at(base + offset_of!(AerogpuCmdDrawIndexed, instance_count), instance_count);
        self.write_u32_at(base + offset_of!(AerogpuCmdDrawIndexed, first_index), first_index);
        self.write_i32_at(base + offset_of!(AerogpuCmdDrawIndexed, base_vertex), base_vertex);
        self.write_u32_at(base + offset_of!(AerogpuCmdDrawIndexed, first_instance), first_instance);
    }

    pub fn present(&mut self, scanout_id: u32, flags: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::Present, size_of::<AerogpuCmdPresent>());
        self.write_u32_at(base + offset_of!(AerogpuCmdPresent, scanout_id), scanout_id);
        self.write_u32_at(base + offset_of!(AerogpuCmdPresent, flags), flags);
    }

    pub fn present_ex(&mut self, scanout_id: u32, flags: u32, d3d9_present_flags: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::PresentEx, size_of::<AerogpuCmdPresentEx>());
        self.write_u32_at(base + offset_of!(AerogpuCmdPresentEx, scanout_id), scanout_id);
        self.write_u32_at(base + offset_of!(AerogpuCmdPresentEx, flags), flags);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdPresentEx, d3d9_present_flags),
            d3d9_present_flags,
        );
    }

    pub fn export_shared_surface(&mut self, resource_handle: AerogpuHandle, share_token: u64) {
        let base = self.append_raw(
            AerogpuCmdOpcode::ExportSharedSurface,
            size_of::<AerogpuCmdExportSharedSurface>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdExportSharedSurface, resource_handle),
            resource_handle,
        );
        self.write_u64_at(base + offset_of!(AerogpuCmdExportSharedSurface, share_token), share_token);
    }

    pub fn import_shared_surface(&mut self, out_resource_handle: AerogpuHandle, share_token: u64) {
        let base = self.append_raw(
            AerogpuCmdOpcode::ImportSharedSurface,
            size_of::<AerogpuCmdImportSharedSurface>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdImportSharedSurface, out_resource_handle),
            out_resource_handle,
        );
        self.write_u64_at(base + offset_of!(AerogpuCmdImportSharedSurface, share_token), share_token);
    }

    pub fn flush(&mut self) {
        let _base = self.append_raw(AerogpuCmdOpcode::Flush, size_of::<AerogpuCmdFlush>());
    }
}
