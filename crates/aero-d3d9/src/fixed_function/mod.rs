pub mod fvf;
pub mod shader_gen;
pub mod tss;

use std::collections::HashMap;
use std::sync::Arc;

use shader_gen::{
    generate_fixed_function_shaders, FixedFunctionShaderDesc, GeneratedFixedFunctionShaders,
};

/// Cache of fixed-function WGSL generation results, keyed by a deterministic state hash.
#[derive(Default)]
pub struct FixedFunctionShaderCache {
    shaders: HashMap<u64, Arc<GeneratedFixedFunctionShaders>>,
    hits: u64,
    misses: u64,
}

impl FixedFunctionShaderCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn get_or_create(
        &mut self,
        desc: &FixedFunctionShaderDesc,
    ) -> Arc<GeneratedFixedFunctionShaders> {
        let hash = desc.state_hash();
        if let Some(existing) = self.shaders.get(&hash) {
            self.hits += 1;
            return Arc::clone(existing);
        }

        let generated = Arc::new(generate_fixed_function_shaders(desc));
        self.shaders.insert(hash, Arc::clone(&generated));
        self.misses += 1;
        generated
    }
}
