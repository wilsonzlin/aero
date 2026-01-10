use std::collections::HashMap;

/// A tiny WGSL shader library with module caching.
///
/// WebGPU shader module compilation is expensive. Higher layers should compile
/// once per unique WGSL source and reuse the module handles.
#[derive(Default)]
pub struct ShaderLibrary {
    /// Named WGSL sources. This is optional; callers can also use [`get_or_create`]
    /// with an inline WGSL string.
    sources: HashMap<String, String>,
    modules: HashMap<u64, wgpu::ShaderModule>,
}

impl ShaderLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_wgsl(&mut self, name: impl Into<String>, wgsl: impl Into<String>) {
        self.sources.insert(name.into(), wgsl.into());
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.sources.get(name).map(String::as_str)
    }

    pub fn get_or_create(&mut self, device: &wgpu::Device, wgsl: &str, label: Option<&str>) -> &wgpu::ShaderModule {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        wgsl.hash(&mut hasher);
        let key = hasher.finish();

        self.modules.entry(key).or_insert_with(|| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label,
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
            })
        })
    }
}

