use std::fmt;

/// Shader stages exposed by the AeroGPU D3D10/11 command executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    Vertex,
    Pixel,
    Compute,
}

impl ShaderStage {
    pub const fn from_aerogpu_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Vertex),
            1 => Some(Self::Pixel),
            2 => Some(Self::Compute),
            _ => None,
        }
    }

    pub const fn as_bind_group_index(self) -> u32 {
        match self {
            Self::Vertex => 0,
            Self::Pixel => 1,
            Self::Compute => 2,
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
        if self.srv_buffers.get(slot_usize).is_some_and(|v| v.is_some()) {
            self.srv_buffers.resize(slot_usize + 1, None);
            if self.srv_buffers[slot_usize].is_some() {
                self.srv_buffers[slot_usize] = None;
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
        if self.textures.get(slot_usize).is_some_and(|v| v.is_some()) {
            self.textures.resize(slot_usize + 1, None);
            if self.textures[slot_usize].is_some() {
                self.textures[slot_usize] = None;
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
        if self.uav_textures.get(slot_usize).is_some_and(|v| v.is_some()) {
            self.uav_textures.resize(slot_usize + 1, None);
            if self.uav_textures[slot_usize].is_some() {
                self.uav_textures[slot_usize] = None;
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
        if self.uav_buffers.get(slot_usize).is_some_and(|v| v.is_some()) {
            self.uav_buffers.resize(slot_usize + 1, None);
            if self.uav_buffers[slot_usize].is_some() {
                self.uav_buffers[slot_usize] = None;
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
    compute: StageBindings,
}

impl BindingState {
    pub fn stage(&self, stage: ShaderStage) -> &StageBindings {
        match stage {
            ShaderStage::Vertex => &self.vertex,
            ShaderStage::Pixel => &self.pixel,
            ShaderStage::Compute => &self.compute,
        }
    }

    pub fn stage_mut(&mut self, stage: ShaderStage) -> &mut StageBindings {
        match stage {
            ShaderStage::Vertex => &mut self.vertex,
            ShaderStage::Pixel => &mut self.pixel,
            ShaderStage::Compute => &mut self.compute,
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.vertex.dirty = true;
        self.pixel.dirty = true;
        self.compute.dirty = true;
    }
}

impl fmt::Display for ShaderStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShaderStage::Vertex => write!(f, "vertex"),
            ShaderStage::Pixel => write!(f, "pixel"),
            ShaderStage::Compute => write!(f, "compute"),
        }
    }
}
