#![forbid(unsafe_code)]

// The full implementation is only meaningful on wasm32.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use aero_d3d11::runtime::{
        D3D11_TRANSLATOR_CACHE_VERSION, PersistedShaderArtifact, PersistedShaderStage, ShaderCache,
        ShaderCacheSource, ShaderTranslationFlags,
    };
    use aero_d3d11::{
        DxbcFile, ShaderReflection, Sm4Program, parse_signatures, translate_sm4_module_to_wgsl,
    };
    use serde::Serialize;
    use wasm_bindgen::prelude::*;

    const DEMO_DXBC: &[u8] = include_bytes!("../../aero-d3d11/tests/fixtures/vs_passthrough.dxbc");

    #[derive(Clone, Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct DemoResult {
        pub translate_calls: u64,
        pub persistent_hits: u64,
        pub persistent_misses: u64,
        pub cache_disabled: bool,
        pub source: String,
        pub d3d11_translator_cache_version: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub caps_hash: Option<String>,
    }

    #[wasm_bindgen]
    pub async fn run_d3d11_shader_cache_demo(
        caps_hash: Option<String>,
    ) -> Result<JsValue, JsValue> {
        let flags = ShaderTranslationFlags::new(caps_hash.clone());
        let mut cache = ShaderCache::new();

        let (artifact, source) = cache
            .get_or_translate_with_source(DEMO_DXBC, flags.clone(), || async {
                // This demo runs the real DXBC -> WGSL translation on cache miss.
                let dxbc = DxbcFile::parse(DEMO_DXBC).map_err(|e| e.to_string())?;
                let program = Sm4Program::parse_from_dxbc(&dxbc).map_err(|e| e.to_string())?;

                // Note: for the demo shader we expect VS/PS; compute is not translated yet.
                let stage = match program.stage {
                    aero_d3d11::ShaderStage::Vertex => PersistedShaderStage::Vertex,
                    aero_d3d11::ShaderStage::Pixel => PersistedShaderStage::Pixel,
                    aero_d3d11::ShaderStage::Compute => PersistedShaderStage::Compute,
                    aero_d3d11::ShaderStage::Geometry
                    | aero_d3d11::ShaderStage::Hull
                    | aero_d3d11::ShaderStage::Domain
                    | aero_d3d11::ShaderStage::Unknown(_) => PersistedShaderStage::Ignored,
                };

                if stage == PersistedShaderStage::Ignored {
                    return Ok(PersistedShaderArtifact {
                        wgsl: String::new(),
                        stage,
                        bindings: Vec::new(),
                        vs_input_signature: Vec::new(),
                    });
                }

                let signatures = parse_signatures(&dxbc).map_err(|e| e.to_string())?;
                let signature_driven = signatures.isgn.is_some() && signatures.osgn.is_some();
                let reflection: ShaderReflection;
                let wgsl: String;
                if signature_driven {
                    let module =
                        aero_d3d11::sm4::decode_program(&program).map_err(|e| e.to_string())?;
                    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
                        .map_err(|e| e.to_string())?;
                    wgsl = translated.wgsl;
                    reflection = translated.reflection;
                } else {
                    wgsl = aero_d3d11::translate_sm4_to_wgsl_bootstrap(&program)
                        .map_err(|e| e.to_string())?
                        .wgsl;
                    reflection = ShaderReflection::default();
                }

                Ok(PersistedShaderArtifact {
                    wgsl,
                    stage,
                    bindings: reflection
                        .bindings
                        .iter()
                        .map(aero_d3d11::runtime::PersistedBinding::from_binding)
                        .collect(),
                    vs_input_signature: Vec::new(),
                })
            })
            .await?;

        // Avoid unused variable warnings in release builds; this demo is intentionally minimal.
        let _wgsl_len = artifact.wgsl.len();

        let stats = cache.stats();

        let source_s = match source {
            ShaderCacheSource::Memory => "memory",
            ShaderCacheSource::Persistent => "persistent",
            ShaderCacheSource::Translated => "translated",
        }
        .to_string();

        let out = DemoResult {
            translate_calls: stats.translate_calls,
            persistent_hits: stats.persistent_hits,
            persistent_misses: stats.persistent_misses,
            cache_disabled: stats.persistent_disabled,
            source: source_s,
            d3d11_translator_cache_version: D3D11_TRANSLATOR_CACHE_VERSION,
            caps_hash,
        };

        serde_wasm_bindgen::to_value(&out).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
