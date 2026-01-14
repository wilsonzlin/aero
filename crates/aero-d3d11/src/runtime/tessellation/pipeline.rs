//! Tessellation pipeline state (compute-based HS/DS emulation).
//!
//! The actual shader logic is not implemented yet; this module exists to keep all tessellation
//! pipeline state owned by a dedicated runtime subsystem.

#[derive(Debug, Default)]
pub struct TessellationPipelines {
    _private: (),
}

impl TessellationPipelines {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

