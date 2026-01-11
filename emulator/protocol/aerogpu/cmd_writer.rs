//! Safe command stream builder for AeroGPU (`aerogpu_cmd.h`).
//!
//! This is intended for tests/fixtures and host-side tooling that needs to emit
//! canonical command streams (correct packet `size_bytes`, padding/alignment,
//! and stream header bookkeeping).

use core::mem::{offset_of, size_of};

use super::aerogpu_cmd::{
    AerogpuCmdClear, AerogpuCmdCreateBuffer, AerogpuCmdCreateInputLayout, AerogpuCmdCreateShaderDxbc,
    AerogpuCmdCreateTexture2d, AerogpuCmdDestroyInputLayout, AerogpuCmdDestroyResource, AerogpuCmdDestroyShader,
    AerogpuCmdDraw, AerogpuCmdDrawIndexed, AerogpuCmdOpcode, AerogpuCmdPresent, AerogpuCmdResourceDirtyRange,
    AerogpuCmdSetIndexBuffer, AerogpuCmdSetInputLayout, AerogpuCmdSetPrimitiveTopology, AerogpuCmdSetRenderTargets,
    AerogpuCmdSetScissor, AerogpuCmdSetVertexBuffers, AerogpuCmdSetViewport, AerogpuCmdStreamFlags,
    AerogpuCmdStreamHeader, AerogpuCmdUploadResource, AerogpuHandle, AerogpuIndexFormat, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_MAX_RENDER_TARGETS,
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

    pub fn flush(&mut self) {
        use super::aerogpu_cmd::AerogpuCmdFlush;

        let _base = self.append_raw(AerogpuCmdOpcode::Flush, size_of::<AerogpuCmdFlush>());
    }
}
