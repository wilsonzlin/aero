#![forbid(unsafe_code)]

/// GPU backend kind used for pipeline keying and feature gating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BackendKind {
    WebGpu = 0,
    WebGl2 = 1,
}

/// Render target format used for pipeline keying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ColorFormat {
    Rgba8Unorm = 0,
    Bgra8Unorm = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipelineKeyParts {
    pub backend: BackendKind,
    pub color_format: ColorFormat,
    pub sample_count: u8,
    pub has_depth: bool,
}

/// Stable 64-bit key for render pipeline caching.
///
/// This is intentionally compact and deterministic across JS/WASM boundaries.
pub fn pipeline_key(parts: PipelineKeyParts) -> u64 {
    debug_assert!(parts.sample_count > 0, "sample_count must be non-zero");

    let mut key = 0u64;
    key |= parts.backend as u64;
    key |= (parts.color_format as u64) << 2;
    key |= (parts.sample_count as u64) << 4;
    key |= (parts.has_depth as u64) << 12;
    key
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShaderValidationError {
    MissingVertexStage,
    MissingFragmentStage,
    MultipleVertexStages,
    MultipleFragmentStages,
}

/// Very small WGSL sanity check intended for *unit testing* and pipeline cache safety.
///
/// GPU execution tests should happen in real browsers (e.g. Playwright); this helper exists
/// to catch obvious errors early in non-GPU test environments.
pub fn validate_wgsl_render_shader(source: &str) -> Result<(), ShaderValidationError> {
    let vertex = source.matches("@vertex").count();
    let fragment = source.matches("@fragment").count();

    if vertex == 0 {
        return Err(ShaderValidationError::MissingVertexStage);
    }
    if fragment == 0 {
        return Err(ShaderValidationError::MissingFragmentStage);
    }
    if vertex > 1 {
        return Err(ShaderValidationError::MultipleVertexStages);
    }
    if fragment > 1 {
        return Err(ShaderValidationError::MultipleFragmentStages);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_key_changes_when_inputs_change() {
        let a = pipeline_key(PipelineKeyParts {
            backend: BackendKind::WebGpu,
            color_format: ColorFormat::Bgra8Unorm,
            sample_count: 1,
            has_depth: false,
        });

        let b = pipeline_key(PipelineKeyParts {
            backend: BackendKind::WebGl2,
            color_format: ColorFormat::Bgra8Unorm,
            sample_count: 1,
            has_depth: false,
        });

        assert_ne!(a, b);
    }

    #[test]
    fn validate_wgsl_accepts_single_vertex_and_fragment_stage() {
        let wgsl = r#"
            @vertex
            fn vs_main() -> @builtin(position) vec4<f32> {
              return vec4<f32>(0.0, 0.0, 0.0, 1.0);
            }

            @fragment
            fn fs_main() -> @location(0) vec4<f32> {
              return vec4<f32>(1.0, 0.0, 0.0, 1.0);
            }
        "#;

        assert_eq!(validate_wgsl_render_shader(wgsl), Ok(()));
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn pipeline_key_is_stable_in_wasm() {
        let key = pipeline_key(PipelineKeyParts {
            backend: BackendKind::WebGpu,
            color_format: ColorFormat::Bgra8Unorm,
            sample_count: 4,
            has_depth: true,
        });

        // The exact numeric value is part of our "public contract": it must stay stable across
        // refactors to avoid invalidating any persistent caches.
        assert_eq!(key, 0b1_0000_0000_0000u64 | 0b0100_0000u64 | 0b0100u64);
    }

    #[wasm_bindgen_test]
    fn validate_wgsl_errors_are_wasm_safe() {
        assert_eq!(
            validate_wgsl_render_shader("@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(); }"),
            Err(ShaderValidationError::MissingVertexStage),
        );
    }
}
