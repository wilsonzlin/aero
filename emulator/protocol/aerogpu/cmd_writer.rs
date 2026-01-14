//! Safe command stream builder for AeroGPU (`aerogpu_cmd.h`).
//!
//! This is intended for tests/fixtures and host-side tooling that needs to emit
//! canonical command streams (correct packet `size_bytes`, padding/alignment,
//! and stream header bookkeeping).

use core::mem::{offset_of, size_of};

use super::aerogpu_cmd::{
    encode_stage_ex, AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdClear, AerogpuCmdCopyBuffer,
    AerogpuCmdCopyTexture2d, AerogpuCmdCreateBuffer, AerogpuCmdCreateInputLayout,
    AerogpuCmdCreateSampler, AerogpuCmdCreateShaderDxbc, AerogpuCmdCreateTexture2d,
    AerogpuCmdDestroyInputLayout, AerogpuCmdDestroyResource, AerogpuCmdDestroySampler,
    AerogpuCmdDestroyShader, AerogpuCmdDispatch, AerogpuCmdDraw, AerogpuCmdDrawIndexed,
    AerogpuCmdExportSharedSurface, AerogpuCmdFlush, AerogpuCmdImportSharedSurface,
    AerogpuCmdOpcode, AerogpuCmdPresent, AerogpuCmdPresentEx, AerogpuCmdReleaseSharedSurface,
    AerogpuCmdResourceDirtyRange, AerogpuCmdSetBlendState, AerogpuCmdSetConstantBuffers,
    AerogpuCmdSetDepthStencilState, AerogpuCmdSetIndexBuffer, AerogpuCmdSetInputLayout,
    AerogpuCmdSetPrimitiveTopology, AerogpuCmdSetRasterizerState, AerogpuCmdSetRenderState,
    AerogpuCmdSetRenderTargets, AerogpuCmdSetSamplerState, AerogpuCmdSetSamplers,
    AerogpuCmdSetScissor, AerogpuCmdSetShaderConstantsF, AerogpuCmdSetShaderResourceBuffers,
    AerogpuCmdSetTexture, AerogpuCmdSetUnorderedAccessBuffers, AerogpuCmdSetVertexBuffers,
    AerogpuCmdSetViewport, AerogpuCmdStreamFlags, AerogpuCmdStreamHeader, AerogpuCmdUploadResource,
    AerogpuCompareFunc, AerogpuConstantBufferBinding, AerogpuCullMode, AerogpuFillMode,
    AerogpuHandle, AerogpuIndexFormat, AerogpuPrimitiveTopology, AerogpuSamplerAddressMode,
    AerogpuSamplerFilter, AerogpuShaderResourceBufferBinding, AerogpuShaderStage,
    AerogpuShaderStageEx, AerogpuUnorderedAccessBufferBinding, AerogpuVertexBufferBinding,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_WRITEBACK_DST, AEROGPU_MAX_RENDER_TARGETS,
};
use super::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

/// WebGPU requires buffer copy offsets and sizes be 4-byte aligned.
///
/// This matches `wgpu::COPY_BUFFER_ALIGNMENT` but we avoid depending on `wgpu` in the
/// protocol crate.
const COPY_BUFFER_ALIGNMENT: u64 = 4;

fn align_up(v: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    let mask = a - 1;
    v.checked_add(mask)
        .unwrap_or_else(|| panic!("align_up overflow: v={v} a={a}"))
        & !mask
}

fn encode_shader_stage_with_ex(
    shader_stage: AerogpuShaderStage,
    stage_ex: Option<AerogpuShaderStageEx>,
) -> (u32, u32) {
    match stage_ex {
        Some(stage_ex) => encode_stage_ex(stage_ex),
        None => (shader_stage as u32, 0),
    }
}

/// Safe command stream builder for `aerogpu_cmd.h`.
#[derive(Debug, Clone)]
pub struct AerogpuCmdWriter {
    buf: Vec<u8>,
}

impl Default for AerogpuCmdWriter {
    fn default() -> Self {
        Self::new()
    }
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
        let new_len = offset
            .checked_add(aligned_size)
            .expect("command stream too large for usize");
        assert!(
            new_len <= u32::MAX as usize,
            "command stream too large for u32 size_bytes"
        );
        self.buf.resize(new_len, 0);

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
        assert!(
            size_bytes.is_multiple_of(COPY_BUFFER_ALIGNMENT),
            "CREATE_BUFFER size_bytes must be {COPY_BUFFER_ALIGNMENT}-byte aligned (got {size_bytes})",
        );
        let base = self.append_raw(
            AerogpuCmdOpcode::CreateBuffer,
            size_of::<AerogpuCmdCreateBuffer>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateBuffer, buffer_handle),
            buffer_handle,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateBuffer, usage_flags),
            usage_flags,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdCreateBuffer, size_bytes),
            size_bytes,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateBuffer, backing_alloc_id),
            backing_alloc_id,
        );
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
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateTexture2d, texture_handle),
            texture_handle,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateTexture2d, usage_flags),
            usage_flags,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, format), format);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, width), width);
        self.write_u32_at(base + offset_of!(AerogpuCmdCreateTexture2d, height), height);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateTexture2d, mip_levels),
            mip_levels,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateTexture2d, array_layers),
            array_layers,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateTexture2d, row_pitch_bytes),
            row_pitch_bytes,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateTexture2d, backing_alloc_id),
            backing_alloc_id,
        );
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
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDestroyResource, resource_handle),
            resource_handle,
        );
    }

    pub fn resource_dirty_range(
        &mut self,
        resource_handle: AerogpuHandle,
        offset_bytes: u64,
        size_bytes: u64,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::ResourceDirtyRange,
            size_of::<AerogpuCmdResourceDirtyRange>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdResourceDirtyRange, resource_handle),
            resource_handle,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdResourceDirtyRange, offset_bytes),
            offset_bytes,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdResourceDirtyRange, size_bytes),
            size_bytes,
        );
    }

    pub fn upload_resource(
        &mut self,
        resource_handle: AerogpuHandle,
        offset_bytes: u64,
        data: &[u8],
    ) {
        let unpadded_size = size_of::<AerogpuCmdUploadResource>()
            .checked_add(data.len())
            .expect("UPLOAD_RESOURCE packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::UploadResource, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdUploadResource, resource_handle),
            resource_handle,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdUploadResource, offset_bytes),
            offset_bytes,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdUploadResource, size_bytes),
            data.len() as u64,
        );
        self.buf[base + size_of::<AerogpuCmdUploadResource>()
            ..base + size_of::<AerogpuCmdUploadResource>() + data.len()]
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
        assert!(
            dst_offset_bytes.is_multiple_of(COPY_BUFFER_ALIGNMENT)
                && src_offset_bytes.is_multiple_of(COPY_BUFFER_ALIGNMENT)
                && size_bytes.is_multiple_of(COPY_BUFFER_ALIGNMENT),
            "COPY_BUFFER offsets and size must be {COPY_BUFFER_ALIGNMENT}-byte aligned (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes} size_bytes={size_bytes})",
        );
        let base = self.append_raw(
            AerogpuCmdOpcode::CopyBuffer,
            size_of::<AerogpuCmdCopyBuffer>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyBuffer, dst_buffer),
            dst_buffer,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyBuffer, src_buffer),
            src_buffer,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdCopyBuffer, dst_offset_bytes),
            dst_offset_bytes,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdCopyBuffer, src_offset_bytes),
            src_offset_bytes,
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdCopyBuffer, size_bytes),
            size_bytes,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyBuffer, flags), flags);
    }

    pub fn copy_buffer_writeback_dst(
        &mut self,
        dst_buffer: AerogpuHandle,
        src_buffer: AerogpuHandle,
        dst_offset_bytes: u64,
        src_offset_bytes: u64,
        size_bytes: u64,
    ) {
        self.copy_buffer(
            dst_buffer,
            src_buffer,
            dst_offset_bytes,
            src_offset_bytes,
            size_bytes,
            AEROGPU_COPY_FLAG_WRITEBACK_DST,
        );
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
        let base = self.append_raw(
            AerogpuCmdOpcode::CopyTexture2d,
            size_of::<AerogpuCmdCopyTexture2d>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyTexture2d, dst_texture),
            dst_texture,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyTexture2d, src_texture),
            src_texture,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyTexture2d, dst_mip_level),
            dst_mip_level,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyTexture2d, dst_array_layer),
            dst_array_layer,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyTexture2d, src_mip_level),
            src_mip_level,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCopyTexture2d, src_array_layer),
            src_array_layer,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, dst_x), dst_x);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, dst_y), dst_y);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, src_x), src_x);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, src_y), src_y);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, width), width);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, height), height);
        self.write_u32_at(base + offset_of!(AerogpuCmdCopyTexture2d, flags), flags);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn copy_texture2d_writeback_dst(
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
    ) {
        self.copy_texture2d(
            dst_texture,
            src_texture,
            dst_mip_level,
            dst_array_layer,
            src_mip_level,
            src_array_layer,
            dst_x,
            dst_y,
            src_x,
            src_y,
            width,
            height,
            AEROGPU_COPY_FLAG_WRITEBACK_DST,
        );
    }

    pub fn create_shader_dxbc(
        &mut self,
        shader_handle: AerogpuHandle,
        stage: AerogpuShaderStage,
        dxbc_bytes: &[u8],
    ) {
        assert!(dxbc_bytes.len() <= u32::MAX as usize);
        let unpadded_size = size_of::<AerogpuCmdCreateShaderDxbc>()
            .checked_add(dxbc_bytes.len())
            .expect("CREATE_SHADER_DXBC packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::CreateShaderDxbc, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, shader_handle),
            shader_handle,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, stage),
            stage as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes),
            dxbc_bytes.len() as u32,
        );
        self.buf[base + size_of::<AerogpuCmdCreateShaderDxbc>()
            ..base + size_of::<AerogpuCmdCreateShaderDxbc>() + dxbc_bytes.len()]
            .copy_from_slice(dxbc_bytes);
    }

    /// Stage-ex aware variant of [`Self::create_shader_dxbc`].
    ///
    /// Encodes `stage_ex` into `reserved0` and sets the legacy `stage` field to `COMPUTE`.
    ///
    /// Note: `stage_ex = 0` (DXBC Pixel program-type) cannot be encoded here because
    /// `reserved0 == 0` is reserved for legacy/default "no stage_ex".
    pub fn create_shader_dxbc_ex(
        &mut self,
        shader_handle: AerogpuHandle,
        stage_ex: AerogpuShaderStageEx,
        dxbc_bytes: &[u8],
    ) {
        // `stage_ex == 0` is reserved for legacy/default (old guests always write 0 into reserved
        // fields). As a result, the DXBC program-type value `0 = Pixel` cannot be encoded into
        // `reserved0` and must use the legacy encoding (`stage = PIXEL`, `reserved0 = 0`) via
        // `create_shader_dxbc` instead.
        if stage_ex == AerogpuShaderStageEx::Pixel {
            panic!("CREATE_SHADER_DXBC stage_ex cannot encode DXBC Pixel program type (0)");
        }

        let (stage, reserved0) = encode_stage_ex(stage_ex);
        assert!(dxbc_bytes.len() <= u32::MAX as usize);
        let unpadded_size = size_of::<AerogpuCmdCreateShaderDxbc>()
            .checked_add(dxbc_bytes.len())
            .expect("CREATE_SHADER_DXBC packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::CreateShaderDxbc, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, shader_handle),
            shader_handle,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, stage),
            stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes),
            dxbc_bytes.len() as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateShaderDxbc, reserved0),
            reserved0,
        );
        self.buf[base + size_of::<AerogpuCmdCreateShaderDxbc>()
            ..base + size_of::<AerogpuCmdCreateShaderDxbc>() + dxbc_bytes.len()]
            .copy_from_slice(dxbc_bytes);
    }

    pub fn destroy_shader(&mut self, shader_handle: AerogpuHandle) {
        let base = self.append_raw(
            AerogpuCmdOpcode::DestroyShader,
            size_of::<AerogpuCmdDestroyShader>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDestroyShader, shader_handle),
            shader_handle,
        );
    }

    /// Bind shaders, including an optional geometry shader.
    ///
    /// ABI note: The on-wire packet layout is unchanged; the `gs` handle is stored in
    /// `AerogpuCmdBindShaders.reserved0` when non-zero.
    pub fn bind_shaders_with_gs(
        &mut self,
        vs: AerogpuHandle,
        gs: AerogpuHandle,
        ps: AerogpuHandle,
        cs: AerogpuHandle,
    ) {
        use super::aerogpu_cmd::AerogpuCmdBindShaders;

        let base = self.append_raw(
            AerogpuCmdOpcode::BindShaders,
            size_of::<AerogpuCmdBindShaders>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, vs), vs);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, ps), ps);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, cs), cs);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, reserved0), gs);
    }

    pub fn bind_shaders(&mut self, vs: AerogpuHandle, ps: AerogpuHandle, cs: AerogpuHandle) {
        self.bind_shaders_with_gs(vs, 0, ps, cs);
    }

    /// Forward-compatible extension of `BIND_SHADERS` that appends GS/HS/DS handles after the
    /// base `struct aerogpu_cmd_bind_shaders`.
    ///
    /// Legacy compatibility: `gs` is also mirrored into the base struct's `reserved0` field so
    /// decoders that only understand the original 24-byte packet can still bind a geometry shader.
    pub fn bind_shaders_ex(
        &mut self,
        vs: AerogpuHandle,
        ps: AerogpuHandle,
        cs: AerogpuHandle,
        gs: AerogpuHandle,
        hs: AerogpuHandle,
        ds: AerogpuHandle,
    ) {
        use super::aerogpu_cmd::AerogpuCmdBindShaders;

        let unpadded_size =
            size_of::<AerogpuCmdBindShaders>() + 3 * size_of::<AerogpuHandle>();
        let base = self.append_raw(AerogpuCmdOpcode::BindShaders, unpadded_size);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, vs), vs);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, ps), ps);
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, cs), cs);
        // Legacy compatibility: keep writing the GS handle into the base struct's `reserved0` so
        // decoders that only understand the original 24-byte `aerogpu_cmd_bind_shaders` layout can
        // still bind a geometry shader.
        self.write_u32_at(base + offset_of!(AerogpuCmdBindShaders, reserved0), gs);

        let ext_base = base + size_of::<AerogpuCmdBindShaders>();
        self.write_u32_at(ext_base + 0 * size_of::<AerogpuHandle>(), gs);
        self.write_u32_at(ext_base + 1 * size_of::<AerogpuHandle>(), hs);
        self.write_u32_at(ext_base + 2 * size_of::<AerogpuHandle>(), ds);
    }

    pub fn create_input_layout(&mut self, input_layout_handle: AerogpuHandle, blob: &[u8]) {
        assert!(blob.len() <= u32::MAX as usize);
        let unpadded_size = size_of::<AerogpuCmdCreateInputLayout>()
            .checked_add(blob.len())
            .expect("CREATE_INPUT_LAYOUT packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::CreateInputLayout, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateInputLayout, input_layout_handle),
            input_layout_handle,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateInputLayout, blob_size_bytes),
            blob.len() as u32,
        );
        self.buf[base + size_of::<AerogpuCmdCreateInputLayout>()
            ..base + size_of::<AerogpuCmdCreateInputLayout>() + blob.len()]
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
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetInputLayout, input_layout_handle),
            input_layout_handle,
        );
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
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetRenderTargets, color_count),
            colors.len() as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetRenderTargets, depth_stencil),
            depth_stencil,
        );

        let colors_base = base + offset_of!(AerogpuCmdSetRenderTargets, colors);
        for (i, &h) in colors.iter().enumerate() {
            self.write_u32_at(colors_base + i * size_of::<AerogpuHandle>(), h);
        }
    }

    pub fn set_viewport(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        min_depth: f32,
        max_depth: f32,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetViewport,
            size_of::<AerogpuCmdSetViewport>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetViewport, x_f32), x.to_bits());
        self.write_u32_at(base + offset_of!(AerogpuCmdSetViewport, y_f32), y.to_bits());
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetViewport, width_f32),
            width.to_bits(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetViewport, height_f32),
            height.to_bits(),
        );
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
        let base = self.append_raw(
            AerogpuCmdOpcode::SetScissor,
            size_of::<AerogpuCmdSetScissor>(),
        );
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, x), x);
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, y), y);
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, width), width);
        self.write_i32_at(base + offset_of!(AerogpuCmdSetScissor, height), height);
    }

    pub fn set_vertex_buffers(&mut self, start_slot: u32, bindings: &[AerogpuVertexBufferBinding]) {
        assert!(bindings.len() <= u32::MAX as usize);
        let bindings_size = size_of::<AerogpuVertexBufferBinding>()
            .checked_mul(bindings.len())
            .expect("SET_VERTEX_BUFFERS packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetVertexBuffers>()
            .checked_add(bindings_size)
            .expect("SET_VERTEX_BUFFERS packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetVertexBuffers, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetVertexBuffers, start_slot),
            start_slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetVertexBuffers, buffer_count),
            bindings.len() as u32,
        );

        let bindings_base = base + size_of::<AerogpuCmdSetVertexBuffers>();
        for (i, binding) in bindings.iter().enumerate() {
            let b = bindings_base + i * size_of::<AerogpuVertexBufferBinding>();
            self.write_u32_at(
                b + offset_of!(AerogpuVertexBufferBinding, buffer),
                binding.buffer,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuVertexBufferBinding, stride_bytes),
                binding.stride_bytes,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuVertexBufferBinding, offset_bytes),
                binding.offset_bytes,
            );
        }
    }

    pub fn set_index_buffer(
        &mut self,
        buffer: AerogpuHandle,
        format: AerogpuIndexFormat,
        offset_bytes: u32,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetIndexBuffer,
            size_of::<AerogpuCmdSetIndexBuffer>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetIndexBuffer, buffer), buffer);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetIndexBuffer, format),
            format as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetIndexBuffer, offset_bytes),
            offset_bytes,
        );
    }

    pub fn set_primitive_topology(&mut self, topology: AerogpuPrimitiveTopology) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetPrimitiveTopology,
            size_of::<AerogpuCmdSetPrimitiveTopology>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetPrimitiveTopology, topology),
            topology as u32,
        );
    }

    pub fn set_texture(
        &mut self,
        shader_stage: AerogpuShaderStage,
        slot: u32,
        texture: AerogpuHandle,
    ) {
        self.set_texture_stage_ex(shader_stage, None, slot, texture);
    }

    pub fn set_texture_stage_ex(
        &mut self,
        shader_stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
        slot: u32,
        texture: AerogpuHandle,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetTexture,
            size_of::<AerogpuCmdSetTexture>(),
        );
        let (shader_stage, reserved0) = encode_shader_stage_with_ex(shader_stage, stage_ex);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetTexture, shader_stage),
            shader_stage,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, slot), slot);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, texture), texture);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, reserved0), reserved0);
    }

    /// Stage-ex aware variant of [`Self::set_texture`].
    ///
    /// Encodes `stage_ex` using the `stage_ex` ABI rules:
    /// - VS/PS/CS use the legacy `shader_stage` field with `reserved0 = 0`.
    /// - GS/HS/DS are encoded as `shader_stage = COMPUTE` with a non-zero `reserved0` tag (2/3/4).
    pub fn set_texture_ex(
        &mut self,
        stage_ex: AerogpuShaderStageEx,
        slot: u32,
        texture: AerogpuHandle,
    ) {
        let (shader_stage, reserved0) = encode_stage_ex(stage_ex);
        let base = self.append_raw(
            AerogpuCmdOpcode::SetTexture,
            size_of::<AerogpuCmdSetTexture>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetTexture, shader_stage),
            shader_stage,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, slot), slot);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetTexture, texture), texture);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetTexture, reserved0),
            reserved0,
        );
    }

    pub fn set_sampler_state(
        &mut self,
        shader_stage: AerogpuShaderStage,
        slot: u32,
        state: u32,
        value: u32,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetSamplerState,
            size_of::<AerogpuCmdSetSamplerState>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplerState, shader_stage),
            shader_stage as u32,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplerState, slot), slot);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplerState, state), state);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplerState, value), value);
    }

    pub fn set_render_state(&mut self, state: u32, value: u32) {
        let base = self.append_raw(
            AerogpuCmdOpcode::SetRenderState,
            size_of::<AerogpuCmdSetRenderState>(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetRenderState, state), state);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetRenderState, value), value);
    }

    pub fn create_sampler(
        &mut self,
        sampler_handle: AerogpuHandle,
        filter: AerogpuSamplerFilter,
        address_u: AerogpuSamplerAddressMode,
        address_v: AerogpuSamplerAddressMode,
        address_w: AerogpuSamplerAddressMode,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::CreateSampler,
            size_of::<AerogpuCmdCreateSampler>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateSampler, sampler_handle),
            sampler_handle,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateSampler, filter),
            filter as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateSampler, address_u),
            address_u as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateSampler, address_v),
            address_v as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdCreateSampler, address_w),
            address_w as u32,
        );
    }

    pub fn destroy_sampler(&mut self, sampler_handle: AerogpuHandle) {
        let base = self.append_raw(
            AerogpuCmdOpcode::DestroySampler,
            size_of::<AerogpuCmdDestroySampler>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDestroySampler, sampler_handle),
            sampler_handle,
        );
    }

    pub fn set_samplers(
        &mut self,
        shader_stage: AerogpuShaderStage,
        start_slot: u32,
        samplers: &[AerogpuHandle],
    ) {
        self.set_samplers_stage_ex(shader_stage, None, start_slot, samplers);
    }

    pub fn set_samplers_stage_ex(
        &mut self,
        shader_stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
        start_slot: u32,
        samplers: &[AerogpuHandle],
    ) {
        assert!(samplers.len() <= u32::MAX as usize);
        let samplers_size = size_of::<AerogpuHandle>()
            .checked_mul(samplers.len())
            .expect("SET_SAMPLERS packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetSamplers>()
            .checked_add(samplers_size)
            .expect("SET_SAMPLERS packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetSamplers, unpadded_size);
        let (shader_stage, reserved0) = encode_shader_stage_with_ex(shader_stage, stage_ex);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, start_slot),
            start_slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, sampler_count),
            samplers.len() as u32,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplers, reserved0), reserved0);

        let payload_base = base + size_of::<AerogpuCmdSetSamplers>();
        for (i, &sampler) in samplers.iter().enumerate() {
            self.write_u32_at(payload_base + i * size_of::<AerogpuHandle>(), sampler);
        }
    }

    /// Stage-ex aware variant of [`Self::set_samplers`].
    ///
    /// Encodes `stage_ex` using the `stage_ex` ABI rules:
    /// - VS/PS/CS use the legacy `shader_stage` field with `reserved0 = 0`.
    /// - GS/HS/DS are encoded as `shader_stage = COMPUTE` with a non-zero `reserved0` tag (2/3/4).
    pub fn set_samplers_ex(
        &mut self,
        stage_ex: AerogpuShaderStageEx,
        start_slot: u32,
        samplers: &[AerogpuHandle],
    ) {
        let (shader_stage, reserved0) = encode_stage_ex(stage_ex);
        assert!(samplers.len() <= u32::MAX as usize);
        let samplers_size = size_of::<AerogpuHandle>()
            .checked_mul(samplers.len())
            .expect("SET_SAMPLERS packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetSamplers>()
            .checked_add(samplers_size)
            .expect("SET_SAMPLERS packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetSamplers, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, start_slot),
            start_slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, sampler_count),
            samplers.len() as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, reserved0),
            reserved0,
        );

        let payload_base = base + size_of::<AerogpuCmdSetSamplers>();
        for (i, &sampler) in samplers.iter().enumerate() {
            self.write_u32_at(payload_base + i * size_of::<AerogpuHandle>(), sampler);
        }
    }

    pub fn set_sampler(
        &mut self,
        shader_stage: AerogpuShaderStage,
        slot: u32,
        sampler: AerogpuHandle,
    ) {
        self.set_sampler_stage_ex(shader_stage, None, slot, sampler);
    }

    pub fn set_sampler_stage_ex(
        &mut self,
        shader_stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
        slot: u32,
        sampler: AerogpuHandle,
    ) {
        let unpadded_size = size_of::<AerogpuCmdSetSamplers>() + size_of::<AerogpuHandle>();
        let base = self.append_raw(AerogpuCmdOpcode::SetSamplers, unpadded_size);
        let (shader_stage, reserved0) = encode_shader_stage_with_ex(shader_stage, stage_ex);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetSamplers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplers, start_slot), slot);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplers, sampler_count), 1);
        self.write_u32_at(base + offset_of!(AerogpuCmdSetSamplers, reserved0), reserved0);

        let payload_base = base + size_of::<AerogpuCmdSetSamplers>();
        self.write_u32_at(payload_base, sampler);
    }

    pub fn set_constant_buffers(
        &mut self,
        shader_stage: AerogpuShaderStage,
        start_slot: u32,
        bindings: &[AerogpuConstantBufferBinding],
    ) {
        self.set_constant_buffers_stage_ex(shader_stage, None, start_slot, bindings);
    }

    pub fn set_constant_buffers_stage_ex(
        &mut self,
        shader_stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
        start_slot: u32,
        bindings: &[AerogpuConstantBufferBinding],
    ) {
        assert!(bindings.len() <= u32::MAX as usize);
        let bindings_size = size_of::<AerogpuConstantBufferBinding>()
            .checked_mul(bindings.len())
            .expect("SET_CONSTANT_BUFFERS packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetConstantBuffers>()
            .checked_add(bindings_size)
            .expect("SET_CONSTANT_BUFFERS packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetConstantBuffers, unpadded_size);
        let (shader_stage, reserved0) = encode_shader_stage_with_ex(shader_stage, stage_ex);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, start_slot),
            start_slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count),
            bindings.len() as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, reserved0),
            reserved0,
        );

        let bindings_base = base + size_of::<AerogpuCmdSetConstantBuffers>();
        for (i, binding) in bindings.iter().enumerate() {
            let b = bindings_base + i * size_of::<AerogpuConstantBufferBinding>();
            self.write_u32_at(
                b + offset_of!(AerogpuConstantBufferBinding, buffer),
                binding.buffer,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuConstantBufferBinding, offset_bytes),
                binding.offset_bytes,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuConstantBufferBinding, size_bytes),
                binding.size_bytes,
            );
        }
    }

    /// Stage-ex aware variant of [`Self::set_constant_buffers`].
    ///
    /// Encodes `stage_ex` using the `stage_ex` ABI rules:
    /// - VS/PS/CS use the legacy `shader_stage` field with `reserved0 = 0`.
    /// - GS/HS/DS are encoded as `shader_stage = COMPUTE` with a non-zero `reserved0` tag (2/3/4).
    pub fn set_constant_buffers_ex(
        &mut self,
        stage_ex: AerogpuShaderStageEx,
        start_slot: u32,
        bindings: &[AerogpuConstantBufferBinding],
    ) {
        let (shader_stage, reserved0) = encode_stage_ex(stage_ex);
        assert!(bindings.len() <= u32::MAX as usize);
        let bindings_size = size_of::<AerogpuConstantBufferBinding>()
            .checked_mul(bindings.len())
            .expect("SET_CONSTANT_BUFFERS packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetConstantBuffers>()
            .checked_add(bindings_size)
            .expect("SET_CONSTANT_BUFFERS packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetConstantBuffers, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, start_slot),
            start_slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count),
            bindings.len() as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, reserved0),
            reserved0,
        );

        let bindings_base = base + size_of::<AerogpuCmdSetConstantBuffers>();
        for (i, binding) in bindings.iter().enumerate() {
            let b = bindings_base + i * size_of::<AerogpuConstantBufferBinding>();
            self.write_u32_at(
                b + offset_of!(AerogpuConstantBufferBinding, buffer),
                binding.buffer,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuConstantBufferBinding, offset_bytes),
                binding.offset_bytes,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuConstantBufferBinding, size_bytes),
                binding.size_bytes,
            );
        }
    }

    pub fn set_shader_resource_buffers(
        &mut self,
        shader_stage: AerogpuShaderStage,
        start_slot: u32,
        bindings: &[AerogpuShaderResourceBufferBinding],
    ) {
        self.set_shader_resource_buffers_stage_ex(shader_stage, None, start_slot, bindings);
    }

    pub fn set_shader_resource_buffers_stage_ex(
        &mut self,
        shader_stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
        start_slot: u32,
        bindings: &[AerogpuShaderResourceBufferBinding],
    ) {
        assert!(bindings.len() <= u32::MAX as usize);
        let (shader_stage, reserved0) = encode_shader_stage_with_ex(shader_stage, stage_ex);
        let bindings_size = size_of::<AerogpuShaderResourceBufferBinding>()
            .checked_mul(bindings.len())
            .expect("SET_SHADER_RESOURCE_BUFFERS packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetShaderResourceBuffers>()
            .checked_add(bindings_size)
            .expect("SET_SHADER_RESOURCE_BUFFERS packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetShaderResourceBuffers, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderResourceBuffers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderResourceBuffers, start_slot),
            start_slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderResourceBuffers, buffer_count),
            bindings.len() as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0),
            reserved0,
        );

        let bindings_base = base + size_of::<AerogpuCmdSetShaderResourceBuffers>();
        for (i, binding) in bindings.iter().enumerate() {
            let b = bindings_base + i * size_of::<AerogpuShaderResourceBufferBinding>();
            self.write_u32_at(
                b + offset_of!(AerogpuShaderResourceBufferBinding, buffer),
                binding.buffer,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuShaderResourceBufferBinding, offset_bytes),
                binding.offset_bytes,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuShaderResourceBufferBinding, size_bytes),
                binding.size_bytes,
            );
        }
    }

    /// Stage-ex aware variant of [`Self::set_shader_resource_buffers`].
    ///
    /// Encodes `stage_ex` into `reserved0` and sets the legacy `shader_stage` to `COMPUTE`.
    ///
    /// Note: `stage_ex = 0` (DXBC Pixel program-type) cannot be encoded here because
    /// `reserved0 == 0` is reserved for legacy/default "no stage_ex".
    pub fn set_shader_resource_buffers_ex(
        &mut self,
        stage_ex: AerogpuShaderStageEx,
        start_slot: u32,
        bindings: &[AerogpuShaderResourceBufferBinding],
    ) {
        self.set_shader_resource_buffers_stage_ex(AerogpuShaderStage::Compute, Some(stage_ex), start_slot, bindings);
    }

    pub fn set_unordered_access_buffers(
        &mut self,
        shader_stage: AerogpuShaderStage,
        start_slot: u32,
        bindings: &[AerogpuUnorderedAccessBufferBinding],
    ) {
        self.set_unordered_access_buffers_stage_ex(shader_stage, None, start_slot, bindings);
    }

    pub fn set_unordered_access_buffers_stage_ex(
        &mut self,
        shader_stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
        start_slot: u32,
        bindings: &[AerogpuUnorderedAccessBufferBinding],
    ) {
        assert!(bindings.len() <= u32::MAX as usize);
        let (shader_stage, reserved0) = encode_shader_stage_with_ex(shader_stage, stage_ex);
        let bindings_size = size_of::<AerogpuUnorderedAccessBufferBinding>()
            .checked_mul(bindings.len())
            .expect("SET_UNORDERED_ACCESS_BUFFERS packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetUnorderedAccessBuffers>()
            .checked_add(bindings_size)
            .expect("SET_UNORDERED_ACCESS_BUFFERS packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetUnorderedAccessBuffers, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, start_slot),
            start_slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, uav_count),
            bindings.len() as u32,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0),
            reserved0,
        );

        let bindings_base = base + size_of::<AerogpuCmdSetUnorderedAccessBuffers>();
        for (i, binding) in bindings.iter().enumerate() {
            let b = bindings_base + i * size_of::<AerogpuUnorderedAccessBufferBinding>();
            self.write_u32_at(
                b + offset_of!(AerogpuUnorderedAccessBufferBinding, buffer),
                binding.buffer,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuUnorderedAccessBufferBinding, offset_bytes),
                binding.offset_bytes,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuUnorderedAccessBufferBinding, size_bytes),
                binding.size_bytes,
            );
            self.write_u32_at(
                b + offset_of!(AerogpuUnorderedAccessBufferBinding, initial_count),
                binding.initial_count,
            );
        }
    }

    /// Stage-ex aware variant of [`Self::set_unordered_access_buffers`].
    ///
    /// Encodes `stage_ex` into `reserved0` and sets the legacy `shader_stage` to `COMPUTE`.
    ///
    /// Note: `stage_ex = 0` (DXBC Pixel program-type) cannot be encoded here because
    /// `reserved0 == 0` is reserved for legacy/default "no stage_ex".
    pub fn set_unordered_access_buffers_ex(
        &mut self,
        stage_ex: AerogpuShaderStageEx,
        start_slot: u32,
        bindings: &[AerogpuUnorderedAccessBufferBinding],
    ) {
        self.set_unordered_access_buffers_stage_ex(
            AerogpuShaderStage::Compute,
            Some(stage_ex),
            start_slot,
            bindings,
        );
    }
    pub fn set_constant_buffer(
        &mut self,
        shader_stage: AerogpuShaderStage,
        slot: u32,
        buffer: AerogpuHandle,
        offset_bytes: u32,
        size_bytes: u32,
    ) {
        self.set_constant_buffer_stage_ex(shader_stage, None, slot, buffer, offset_bytes, size_bytes);
    }

    pub fn set_constant_buffer_stage_ex(
        &mut self,
        shader_stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
        slot: u32,
        buffer: AerogpuHandle,
        offset_bytes: u32,
        size_bytes: u32,
    ) {
        let unpadded_size =
            size_of::<AerogpuCmdSetConstantBuffers>() + size_of::<AerogpuConstantBufferBinding>();
        let base = self.append_raw(AerogpuCmdOpcode::SetConstantBuffers, unpadded_size);
        let (shader_stage, reserved0) = encode_shader_stage_with_ex(shader_stage, stage_ex);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage),
            shader_stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, start_slot),
            slot,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count),
            1,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetConstantBuffers, reserved0),
            reserved0,
        );

        let binding_base = base + size_of::<AerogpuCmdSetConstantBuffers>();
        self.write_u32_at(
            binding_base + offset_of!(AerogpuConstantBufferBinding, buffer),
            buffer,
        );
        self.write_u32_at(
            binding_base + offset_of!(AerogpuConstantBufferBinding, offset_bytes),
            offset_bytes,
        );
        self.write_u32_at(
            binding_base + offset_of!(AerogpuConstantBufferBinding, size_bytes),
            size_bytes,
        );
    }

    pub fn set_shader_constants_f(
        &mut self,
        stage: AerogpuShaderStage,
        start_register: u32,
        data: &[f32],
    ) {
        self.set_shader_constants_f_stage_ex(stage, None, start_register, data);
    }

    pub fn set_shader_constants_f_stage_ex(
        &mut self,
        stage: AerogpuShaderStage,
        stage_ex: Option<AerogpuShaderStageEx>,
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
        let payload_size = data
            .len()
            .checked_mul(4)
            .expect("SET_SHADER_CONSTANTS_F packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetShaderConstantsF>()
            .checked_add(payload_size)
            .expect("SET_SHADER_CONSTANTS_F packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetShaderConstantsF, unpadded_size);
        let (stage, reserved0) = encode_shader_stage_with_ex(stage, stage_ex);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, stage),
            stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, start_register),
            start_register,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, vec4_count),
            vec4_count,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0),
            reserved0,
        );

        let payload_base = base + size_of::<AerogpuCmdSetShaderConstantsF>();
        for (i, &v) in data.iter().enumerate() {
            self.write_u32_at(payload_base + i * 4, v.to_bits());
        }
    }

    /// Stage-ex aware variant of [`Self::set_shader_constants_f`].
    ///
    /// Encodes `stage_ex` using the `stage_ex` ABI rules:
    /// - VS/PS/CS use the legacy `stage` field with `reserved0 = 0`.
    /// - GS/HS/DS are encoded as `stage = COMPUTE` with a non-zero `reserved0` tag (2/3/4).
    pub fn set_shader_constants_f_ex(
        &mut self,
        stage_ex: AerogpuShaderStageEx,
        start_register: u32,
        data: &[f32],
    ) {
        let (stage, reserved0) = encode_stage_ex(stage_ex);
        assert_eq!(
            data.len() % 4,
            0,
            "SET_SHADER_CONSTANTS_F data must be float4-aligned (got {} floats)",
            data.len()
        );
        assert!(data.len() <= u32::MAX as usize);

        let vec4_count = (data.len() / 4) as u32;
        let payload_size = data
            .len()
            .checked_mul(4)
            .expect("SET_SHADER_CONSTANTS_F packet too large (usize overflow)");
        let unpadded_size = size_of::<AerogpuCmdSetShaderConstantsF>()
            .checked_add(payload_size)
            .expect("SET_SHADER_CONSTANTS_F packet too large (usize overflow)");
        let base = self.append_raw(AerogpuCmdOpcode::SetShaderConstantsF, unpadded_size);
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, stage),
            stage,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, start_register),
            start_register,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, vec4_count),
            vec4_count,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0),
            reserved0,
        );

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
        self.set_blend_state_ext(
            enable,
            src_factor,
            dst_factor,
            blend_op,
            src_factor,
            dst_factor,
            blend_op,
            [1.0; 4],
            0xFFFF_FFFF,
            color_write_mask,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_blend_state_ext(
        &mut self,
        enable: bool,
        src_factor: AerogpuBlendFactor,
        dst_factor: AerogpuBlendFactor,
        blend_op: AerogpuBlendOp,
        src_factor_alpha: AerogpuBlendFactor,
        dst_factor_alpha: AerogpuBlendFactor,
        blend_op_alpha: AerogpuBlendOp,
        blend_constant_rgba: [f32; 4],
        sample_mask: u32,
        color_write_mask: u8,
    ) {
        use super::aerogpu_cmd::AerogpuBlendState;

        let base = self.append_raw(
            AerogpuCmdOpcode::SetBlendState,
            size_of::<AerogpuCmdSetBlendState>(),
        );
        let state_base = base + offset_of!(AerogpuCmdSetBlendState, state);
        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, enable),
            enable as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, src_factor),
            src_factor as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, dst_factor),
            dst_factor as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, blend_op),
            blend_op as u32,
        );
        self.write_u8_at(
            state_base + offset_of!(AerogpuBlendState, color_write_mask),
            color_write_mask,
        );

        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, src_factor_alpha),
            src_factor_alpha as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, dst_factor_alpha),
            dst_factor_alpha as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, blend_op_alpha),
            blend_op_alpha as u32,
        );
        let constant_base = state_base + offset_of!(AerogpuBlendState, blend_constant_rgba_f32);
        for (i, c) in blend_constant_rgba.iter().enumerate() {
            self.write_u32_at(constant_base + i * 4, c.to_bits());
        }
        self.write_u32_at(
            state_base + offset_of!(AerogpuBlendState, sample_mask),
            sample_mask,
        );
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
        self.write_u32_at(
            state_base + offset_of!(AerogpuDepthStencilState, depth_func),
            depth_func as u32,
        );
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
        flags: u32,
    ) {
        use super::aerogpu_cmd::AerogpuRasterizerState;

        let base = self.append_raw(
            AerogpuCmdOpcode::SetRasterizerState,
            size_of::<AerogpuCmdSetRasterizerState>(),
        );
        let state_base = base + offset_of!(AerogpuCmdSetRasterizerState, state);
        self.write_u32_at(
            state_base + offset_of!(AerogpuRasterizerState, fill_mode),
            fill_mode as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuRasterizerState, cull_mode),
            cull_mode as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuRasterizerState, front_ccw),
            front_ccw as u32,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuRasterizerState, scissor_enable),
            scissor_enable as u32,
        );
        self.write_i32_at(
            state_base + offset_of!(AerogpuRasterizerState, depth_bias),
            depth_bias,
        );
        self.write_u32_at(
            state_base + offset_of!(AerogpuRasterizerState, flags),
            flags,
        );
    }

    pub fn set_rasterizer_state_ext(
        &mut self,
        fill_mode: AerogpuFillMode,
        cull_mode: AerogpuCullMode,
        front_ccw: bool,
        scissor_enable: bool,
        depth_bias: i32,
        depth_clip_disable: bool,
    ) {
        use super::aerogpu_cmd::AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE;

        let flags = if depth_clip_disable {
            AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE
        } else {
            0
        };
        self.set_rasterizer_state(
            fill_mode,
            cull_mode,
            front_ccw,
            scissor_enable,
            depth_bias,
            flags,
        );
    }

    pub fn clear(&mut self, flags: u32, color_rgba: [f32; 4], depth: f32, stencil: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::Clear, size_of::<AerogpuCmdClear>());
        self.write_u32_at(base + offset_of!(AerogpuCmdClear, flags), flags);

        let color_base = base + offset_of!(AerogpuCmdClear, color_rgba_f32);
        for (i, c) in color_rgba.iter().enumerate() {
            self.write_u32_at(color_base + i * 4, c.to_bits());
        }

        self.write_u32_at(
            base + offset_of!(AerogpuCmdClear, depth_f32),
            depth.to_bits(),
        );
        self.write_u32_at(base + offset_of!(AerogpuCmdClear, stencil), stencil);
    }

    pub fn draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        let base = self.append_raw(AerogpuCmdOpcode::Draw, size_of::<AerogpuCmdDraw>());
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDraw, vertex_count),
            vertex_count,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDraw, instance_count),
            instance_count,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDraw, first_vertex),
            first_vertex,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDraw, first_instance),
            first_instance,
        );
    }

    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    ) {
        let base = self.append_raw(
            AerogpuCmdOpcode::DrawIndexed,
            size_of::<AerogpuCmdDrawIndexed>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDrawIndexed, index_count),
            index_count,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDrawIndexed, instance_count),
            instance_count,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDrawIndexed, first_index),
            first_index,
        );
        self.write_i32_at(
            base + offset_of!(AerogpuCmdDrawIndexed, base_vertex),
            base_vertex,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDrawIndexed, first_instance),
            first_instance,
        );
    }

    pub fn dispatch(&mut self, group_count_x: u32, group_count_y: u32, group_count_z: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::Dispatch, size_of::<AerogpuCmdDispatch>());
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDispatch, group_count_x),
            group_count_x,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDispatch, group_count_y),
            group_count_y,
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdDispatch, group_count_z),
            group_count_z,
        );
    }

    pub fn present(&mut self, scanout_id: u32, flags: u32) {
        let base = self.append_raw(AerogpuCmdOpcode::Present, size_of::<AerogpuCmdPresent>());
        self.write_u32_at(base + offset_of!(AerogpuCmdPresent, scanout_id), scanout_id);
        self.write_u32_at(base + offset_of!(AerogpuCmdPresent, flags), flags);
    }

    pub fn present_ex(&mut self, scanout_id: u32, flags: u32, d3d9_present_flags: u32) {
        let base = self.append_raw(
            AerogpuCmdOpcode::PresentEx,
            size_of::<AerogpuCmdPresentEx>(),
        );
        self.write_u32_at(
            base + offset_of!(AerogpuCmdPresentEx, scanout_id),
            scanout_id,
        );
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
        self.write_u64_at(
            base + offset_of!(AerogpuCmdExportSharedSurface, share_token),
            share_token,
        );
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
        self.write_u64_at(
            base + offset_of!(AerogpuCmdImportSharedSurface, share_token),
            share_token,
        );
    }

    pub fn release_shared_surface(&mut self, share_token: u64) {
        let base = self.append_raw(
            AerogpuCmdOpcode::ReleaseSharedSurface,
            size_of::<AerogpuCmdReleaseSharedSurface>(),
        );
        self.write_u64_at(
            base + offset_of!(AerogpuCmdReleaseSharedSurface, share_token),
            share_token,
        );
    }

    pub fn flush(&mut self) {
        let _base = self.append_raw(AerogpuCmdOpcode::Flush, size_of::<AerogpuCmdFlush>());
    }
}
