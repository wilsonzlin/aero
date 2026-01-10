use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

/// A generational, type-safe backend-owned handle.
pub struct Handle<Tag> {
    index: u32,
    generation: u32,
    _tag: PhantomData<Tag>,
}

impl<Tag> Copy for Handle<Tag> {}

impl<Tag> Clone for Handle<Tag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Tag> PartialEq for Handle<Tag> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<Tag> Eq for Handle<Tag> {}

impl<Tag> Hash for Handle<Tag> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl<Tag> Handle<Tag> {
    pub(crate) fn new(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            _tag: PhantomData,
        }
    }

    pub fn index(self) -> u32 {
        self.index
    }

    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl<Tag> core::fmt::Debug for Handle<Tag> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Handle")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

pub enum BufferTag {}
pub enum TextureTag {}
pub enum TextureViewTag {}
pub enum SamplerTag {}
pub enum BindGroupLayoutTag {}
pub enum BindGroupTag {}
pub enum PipelineTag {}
pub enum CommandBufferTag {}

pub type BufferId = Handle<BufferTag>;
pub type TextureId = Handle<TextureTag>;
pub type TextureViewId = Handle<TextureViewTag>;
pub type SamplerId = Handle<SamplerTag>;
pub type BindGroupLayoutId = Handle<BindGroupLayoutTag>;
pub type BindGroupId = Handle<BindGroupTag>;
pub type PipelineId = Handle<PipelineTag>;
pub type CommandBufferId = Handle<CommandBufferTag>;
