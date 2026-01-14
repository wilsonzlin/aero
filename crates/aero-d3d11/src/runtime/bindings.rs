use std::fmt;

/// Shader stages exposed by the AeroGPU D3D10/11 command executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Pixel,
    /// D3D11 geometry shader stage.
    ///
    /// WebGPU does not expose geometry shaders, but the guest can still update per-stage binding
    /// tables (textures/samplers/constant buffers) for GS. We track these bindings separately so
    /// `stage_ex` packets don't accidentally overwrite compute-stage state.
    Geometry,
    /// D3D11 hull shader stage (tessellation control).
    Hull,
    /// D3D11 domain shader stage (tessellation evaluation).
    Domain,
    Compute,
}

impl ShaderStage {
    pub const fn from_aerogpu_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Vertex),
            1 => Some(Self::Pixel),
            2 => Some(Self::Compute),
            3 => Some(Self::Geometry),
            _ => None,
        }
    }

    /// Decode a stage value from the legacy `shader_stage` field plus the `stage_ex` ABI extension.
    ///
    /// The AeroGPU shader stage enum historically only supported VS/PS/CS; newer protocol versions
    /// also include `Geometry = 3`. The command stream can also encode additional D3D11 stages
    /// without
    /// breaking older hosts via:
    /// - `shader_stage == COMPUTE` (2)
    /// - `stage_ex != 0` in a packet's reserved field (opcode-specific)
    ///
    /// The `stage_ex` value uses DXBC program-type numbering (SM4/5 version token `program_type`).
    /// In practice, we accept a superset to keep bindings/state updates robust across host versions:
    /// - 1 = vertex shader (VS, accepted as an alias for legacy `shader_stage = Vertex`)
    /// - 2 = geometry shader (GS)
    /// - 3 = hull shader (HS)
    /// - 4 = domain shader (DS)
    /// - 5 = compute shader (CS, optional alias for legacy/default compute)
    ///
    /// Note: `stage_ex == 0` is reserved for "no override" (legacy/default Compute), so stage-ex
    /// packets cannot encode DXBC program type 0 (Pixel). Vertex shaders should still be encoded
    /// via the legacy `shader_stage = Vertex` value, but some host-side encoders always use the
    /// stage_ex path; accept `stage_ex == 1` for robustness.
    pub const fn from_aerogpu_u32_with_stage_ex(stage: u32, stage_ex: u32) -> Option<Self> {
        match stage {
            0 => Some(Self::Vertex),
            1 => Some(Self::Pixel),
            2 => match stage_ex {
                // `stage_ex == 0` is the legacy/default compute-stage encoding.
                //
                // We also tolerate `stage_ex == 5` (DXBC program type for compute) for older/broken
                // command writers that incorrectly used the DXBC value instead of the reserved 0
                // sentinel in binding packets.
                0 | 5 => Some(Self::Compute),
                1 => Some(Self::Vertex),
                2 => Some(Self::Geometry),
                3 => Some(Self::Hull),
                4 => Some(Self::Domain),
                _ => None,
            },
            3 => Some(Self::Geometry),
            _ => None,
        }
    }

    pub const fn as_bind_group_index(self) -> u32 {
        match self {
            Self::Vertex => 0,
            Self::Pixel => 1,
            // Keep the original ABI ordering (VS=0 PS=1 CS=2). Extended stages share group 3 so the
            // user (D3D) binding model stays within the baseline 4 bind groups (0..=3).
            //
            // Internal/emulation-only pipelines (vertex pulling / expansion scratch) also share
            // `@group(3)` (`BIND_GROUP_INTERNAL_EMULATION`) and use a reserved binding-number range
            // starting at `BINDING_BASE_INTERNAL`.
            Self::Compute => 2,
            Self::Geometry | Self::Hull | Self::Domain => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundConstantBuffer {
    pub buffer: u32,
    pub offset: u64,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundBuffer {
    pub buffer: u32,
    pub offset: u64,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundTexture {
    pub texture: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundSampler {
    pub sampler: u32,
}

#[derive(Debug, Default, Clone)]
pub struct StageBindings {
    constant_buffers: Vec<Option<BoundConstantBuffer>>,
    textures: Vec<Option<BoundTexture>>,
    srv_buffers: Vec<Option<BoundBuffer>>,
    uav_buffers: Vec<Option<BoundBuffer>>,
    uav_textures: Vec<Option<BoundTexture>>,
    samplers: Vec<Option<BoundSampler>>,
    dirty: bool,
}

impl StageBindings {
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    pub fn constant_buffer(&self, slot: u32) -> Option<BoundConstantBuffer> {
        self.constant_buffers.get(slot as usize).and_then(|v| *v)
    }

    pub fn texture(&self, slot: u32) -> Option<BoundTexture> {
        self.textures.get(slot as usize).and_then(|v| *v)
    }

    pub fn srv_buffer(&self, slot: u32) -> Option<BoundBuffer> {
        self.srv_buffers.get(slot as usize).and_then(|v| *v)
    }

    pub fn uav_buffer(&self, slot: u32) -> Option<BoundBuffer> {
        self.uav_buffers.get(slot as usize).and_then(|v| *v)
    }

    pub fn uav_texture(&self, slot: u32) -> Option<BoundTexture> {
        self.uav_textures.get(slot as usize).and_then(|v| *v)
    }

    pub fn sampler(&self, slot: u32) -> Option<BoundSampler> {
        self.samplers.get(slot as usize).and_then(|v| *v)
    }

    pub fn set_constant_buffer(&mut self, slot: u32, value: Option<BoundConstantBuffer>) {
        let slot_usize = slot as usize;
        if self.constant_buffers.len() <= slot_usize {
            self.constant_buffers.resize(slot_usize + 1, None);
        }

        if self.constant_buffers[slot_usize] != value {
            self.constant_buffers[slot_usize] = value;
            self.dirty = true;
        }
    }

    pub fn set_texture(&mut self, slot: u32, texture: Option<u32>) {
        let value = texture.map(|texture| BoundTexture { texture });
        let slot_usize = slot as usize;
        if self.textures.len() <= slot_usize {
            self.textures.resize(slot_usize + 1, None);
        }
        let mut changed = false;
        if self.textures[slot_usize] != value {
            self.textures[slot_usize] = value;
            changed = true;
        }

        // A `t#` register can be either a texture SRV or a buffer SRV. Binding one kind unbinds the
        // other.
        if let Some(buf) = self.srv_buffers.get_mut(slot_usize) {
            if buf.is_some() {
                *buf = None;
                changed = true;
            }
        }

        self.dirty |= changed;
    }

    pub fn set_srv_buffer(&mut self, slot: u32, value: Option<BoundBuffer>) {
        let slot_usize = slot as usize;
        if self.srv_buffers.len() <= slot_usize {
            self.srv_buffers.resize(slot_usize + 1, None);
        }
        let mut changed = false;
        if self.srv_buffers[slot_usize] != value {
            self.srv_buffers[slot_usize] = value;
            changed = true;
        }

        // A `t#` register can be either a texture SRV or a buffer SRV. Binding one kind unbinds the
        // other.
        if let Some(tex) = self.textures.get_mut(slot_usize) {
            if tex.is_some() {
                *tex = None;
                changed = true;
            }
        }

        self.dirty |= changed;
    }

    pub fn set_uav_buffer(&mut self, slot: u32, value: Option<BoundBuffer>) {
        let slot_usize = slot as usize;
        if self.uav_buffers.len() <= slot_usize {
            self.uav_buffers.resize(slot_usize + 1, None);
        }
        let mut changed = false;
        if self.uav_buffers[slot_usize] != value {
            self.uav_buffers[slot_usize] = value;
            changed = true;
        }

        // A `u#` register can be either a texture UAV or a buffer UAV. Binding one kind unbinds the
        // other.
        if let Some(tex) = self.uav_textures.get_mut(slot_usize) {
            if tex.is_some() {
                *tex = None;
                changed = true;
            }
        }

        self.dirty |= changed;
    }

    pub fn set_uav_texture(&mut self, slot: u32, value: Option<BoundTexture>) {
        let slot_usize = slot as usize;
        if self.uav_textures.len() <= slot_usize {
            self.uav_textures.resize(slot_usize + 1, None);
        }
        let mut changed = false;
        if self.uav_textures[slot_usize] != value {
            self.uav_textures[slot_usize] = value;
            changed = true;
        }

        // A `u#` register can be either a texture UAV or a buffer UAV. Binding one kind unbinds the
        // other.
        if let Some(buf) = self.uav_buffers.get_mut(slot_usize) {
            if buf.is_some() {
                *buf = None;
                changed = true;
            }
        }

        self.dirty |= changed;
    }

    pub fn set_sampler(&mut self, slot: u32, value: Option<BoundSampler>) {
        let slot_usize = slot as usize;
        if self.samplers.len() <= slot_usize {
            self.samplers.resize(slot_usize + 1, None);
        }
        if self.samplers[slot_usize] != value {
            self.samplers[slot_usize] = value;
            self.dirty = true;
        }
    }

    pub fn clear_constant_buffer_handle(&mut self, handle: u32) {
        let mut changed = false;
        for slot in &mut self.constant_buffers {
            if slot.is_some_and(|cb| cb.buffer == handle) {
                *slot = None;
                changed = true;
            }
        }
        self.dirty |= changed;
    }

    pub fn clear_texture_handle(&mut self, handle: u32) {
        let mut changed = false;
        for slot in &mut self.textures {
            if slot.is_some_and(|tex| tex.texture == handle) {
                *slot = None;
                changed = true;
            }
        }
        self.dirty |= changed;
    }

    pub fn clear_srv_buffer_handle(&mut self, handle: u32) {
        let mut changed = false;
        for slot in &mut self.srv_buffers {
            if slot.is_some_and(|buf| buf.buffer == handle) {
                *slot = None;
                changed = true;
            }
        }
        self.dirty |= changed;
    }

    pub fn clear_uav_buffer_handle(&mut self, handle: u32) {
        let mut changed = false;
        for slot in &mut self.uav_buffers {
            if slot.is_some_and(|buf| buf.buffer == handle) {
                *slot = None;
                changed = true;
            }
        }
        self.dirty |= changed;
    }

    pub fn clear_uav_texture_handle(&mut self, handle: u32) {
        let mut changed = false;
        for slot in &mut self.uav_textures {
            if slot.is_some_and(|tex| tex.texture == handle) {
                *slot = None;
                changed = true;
            }
        }
        self.dirty |= changed;
    }

    pub fn clear_sampler_handle(&mut self, handle: u32) {
        let mut changed = false;
        for slot in &mut self.samplers {
            if slot.is_some_and(|s| s.sampler == handle) {
                *slot = None;
                changed = true;
            }
        }
        self.dirty |= changed;
    }
}

#[derive(Debug, Default, Clone)]
pub struct BindingState {
    vertex: StageBindings,
    pixel: StageBindings,
    geometry: StageBindings,
    hull: StageBindings,
    domain: StageBindings,
    compute: StageBindings,
}

impl BindingState {
    pub fn stage(&self, stage: ShaderStage) -> &StageBindings {
        match stage {
            ShaderStage::Vertex => &self.vertex,
            ShaderStage::Pixel => &self.pixel,
            ShaderStage::Geometry => &self.geometry,
            ShaderStage::Hull => &self.hull,
            ShaderStage::Domain => &self.domain,
            ShaderStage::Compute => &self.compute,
        }
    }

    pub fn stage_mut(&mut self, stage: ShaderStage) -> &mut StageBindings {
        match stage {
            ShaderStage::Vertex => &mut self.vertex,
            ShaderStage::Pixel => &mut self.pixel,
            ShaderStage::Geometry => &mut self.geometry,
            ShaderStage::Hull => &mut self.hull,
            ShaderStage::Domain => &mut self.domain,
            ShaderStage::Compute => &mut self.compute,
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.vertex.dirty = true;
        self.pixel.dirty = true;
        self.geometry.dirty = true;
        self.hull.dirty = true;
        self.domain.dirty = true;
        self.compute.dirty = true;
    }
}

impl fmt::Display for ShaderStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShaderStage::Vertex => write!(f, "vertex"),
            ShaderStage::Pixel => write!(f, "pixel"),
            ShaderStage::Geometry => write!(f, "geometry"),
            ShaderStage::Hull => write!(f, "hull"),
            ShaderStage::Domain => write!(f, "domain"),
            ShaderStage::Compute => write!(f, "compute"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_ex_compute_is_accepted_as_alias_for_legacy_compute() {
        assert_eq!(
            ShaderStage::from_aerogpu_u32_with_stage_ex(2, 5),
            Some(ShaderStage::Compute)
        );
    }

    #[test]
    fn stage_ex_vertex_is_accepted_as_alias_for_legacy_vertex() {
        // `stage_ex == 1` is the DXBC program type for Vertex. Some command writers always use the
        // stage_ex encoding path (even for VS), so accept it for robustness.
        assert_eq!(
            ShaderStage::from_aerogpu_u32_with_stage_ex(2, 1),
            Some(ShaderStage::Vertex)
        );
        assert_eq!(
            ShaderStage::from_aerogpu_u32_with_stage_ex(2, 0),
            Some(ShaderStage::Compute)
        );

        // Non-compute stages ignore stage_ex (reserved field).
        assert_eq!(
            ShaderStage::from_aerogpu_u32_with_stage_ex(0, 1),
            Some(ShaderStage::Vertex)
        );
    }

    #[test]
    fn stage_ex_decodes_extended_d3d11_shader_stages() {
        assert_eq!(
            ShaderStage::from_aerogpu_u32_with_stage_ex(2, 2),
            Some(ShaderStage::Geometry)
        );
        assert_eq!(
            ShaderStage::from_aerogpu_u32_with_stage_ex(2, 3),
            Some(ShaderStage::Hull)
        );
        assert_eq!(
            ShaderStage::from_aerogpu_u32_with_stage_ex(2, 4),
            Some(ShaderStage::Domain)
        );
    }

    #[test]
    fn srv_texture_and_buffer_are_mutually_exclusive() {
        let mut stage = StageBindings::default();

        stage.set_srv_buffer(
            0,
            Some(BoundBuffer {
                buffer: 1,
                offset: 4,
                size: Some(16),
            }),
        );
        assert!(stage.is_dirty());
        assert!(stage.texture(0).is_none());
        assert_eq!(
            stage.srv_buffer(0),
            Some(BoundBuffer {
                buffer: 1,
                offset: 4,
                size: Some(16)
            })
        );

        stage.clear_dirty();
        stage.set_texture(0, Some(2));
        assert!(stage.is_dirty(), "binding a texture must dirty the stage");
        assert_eq!(stage.texture(0), Some(BoundTexture { texture: 2 }));
        assert!(
            stage.srv_buffer(0).is_none(),
            "binding a texture SRV must unbind a buffer SRV in the same slot"
        );

        stage.clear_dirty();
        stage.set_srv_buffer(
            0,
            Some(BoundBuffer {
                buffer: 3,
                offset: 0,
                size: None,
            }),
        );
        assert!(
            stage.is_dirty(),
            "binding an SRV buffer must dirty the stage"
        );
        assert!(
            stage.texture(0).is_none(),
            "binding a buffer SRV must unbind a texture SRV in the same slot"
        );
        assert_eq!(
            stage.srv_buffer(0),
            Some(BoundBuffer {
                buffer: 3,
                offset: 0,
                size: None
            })
        );

        // Redundant bind should not mark the stage dirty again.
        stage.clear_dirty();
        stage.set_srv_buffer(
            0,
            Some(BoundBuffer {
                buffer: 3,
                offset: 0,
                size: None,
            }),
        );
        assert!(
            !stage.is_dirty(),
            "redundant binding should not dirty the stage"
        );
    }

    #[test]
    fn uav_texture_and_buffer_are_mutually_exclusive() {
        let mut stage = StageBindings::default();

        stage.set_uav_texture(0, Some(BoundTexture { texture: 42 }));
        assert_eq!(stage.uav_texture(0), Some(BoundTexture { texture: 42 }));
        assert!(stage.uav_buffer(0).is_none());

        stage.clear_dirty();
        stage.set_uav_buffer(
            0,
            Some(BoundBuffer {
                buffer: 7,
                offset: 0,
                size: None,
            }),
        );
        assert!(stage.is_dirty());
        assert!(
            stage.uav_texture(0).is_none(),
            "binding a UAV buffer must unbind a UAV texture in the same slot"
        );
        assert_eq!(
            stage.uav_buffer(0),
            Some(BoundBuffer {
                buffer: 7,
                offset: 0,
                size: None
            })
        );

        stage.clear_dirty();
        stage.set_uav_texture(0, Some(BoundTexture { texture: 99 }));
        assert!(stage.is_dirty());
        assert!(
            stage.uav_buffer(0).is_none(),
            "binding a UAV texture must unbind a UAV buffer in the same slot"
        );
        assert_eq!(stage.uav_texture(0), Some(BoundTexture { texture: 99 }));
    }

    #[test]
    fn clear_handles_clears_srv_and_uav_buffers() {
        let mut stage = StageBindings::default();

        stage.set_srv_buffer(
            0,
            Some(BoundBuffer {
                buffer: 5,
                offset: 0,
                size: None,
            }),
        );
        stage.set_uav_buffer(
            1,
            Some(BoundBuffer {
                buffer: 5,
                offset: 0,
                size: None,
            }),
        );
        stage.clear_dirty();

        stage.clear_srv_buffer_handle(5);
        assert!(stage.is_dirty());
        assert!(stage.srv_buffer(0).is_none());
        assert!(
            stage.uav_buffer(1).is_some(),
            "clearing srv buffer handle must not affect uav buffer bindings"
        );

        stage.clear_dirty();
        stage.clear_uav_buffer_handle(5);
        assert!(stage.is_dirty());
        assert!(stage.uav_buffer(1).is_none());
    }
}
