use crate::shader_lib::{hash, wgsl, BuiltinShader};

fn validate_webgl2_subset(shader: BuiltinShader) {
    let source = wgsl(shader);

    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|err| panic!("{shader:?} WGSL parse failed: {err}"));

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    let info = validator
        .validate(&module)
        .unwrap_or_else(|err| panic!("{shader:?} WGSL validation failed: {err:?}"));

    let options = naga::back::glsl::Options {
        version: naga::back::glsl::Version::Embedded {
            version: 300,
            is_webgl: true,
        },
        writer_flags: naga::back::glsl::WriterFlags::empty(),
        binding_map: naga::back::glsl::BindingMap::default(),
        zero_initialize_workgroup_memory: false,
    };

    for (stage, entry_point) in [
        (naga::ShaderStage::Vertex, "vs_main"),
        (naga::ShaderStage::Fragment, "fs_main"),
    ] {
        let pipeline_options = naga::back::glsl::PipelineOptions {
            shader_stage: stage,
            entry_point: entry_point.to_string(),
            multiview: None,
        };

        let mut glsl = String::new();
        let mut writer = naga::back::glsl::Writer::new(
            &mut glsl,
            &module,
            &info,
            &options,
            &pipeline_options,
            naga::proc::BoundsCheckPolicies::default(),
        )
        .unwrap_or_else(|err| panic!("{shader:?} GLSL writer init failed: {err}"));

        writer
            .write()
            .unwrap_or_else(|err| {
                panic!("{shader:?} WGSL -> GLSL ES 300 translation failed ({stage:?}/{entry_point}): {err}")
            });
    }
}

#[test]
fn builtin_shaders_are_valid_wgsl_and_glsl_es_compatible() {
    for shader in BuiltinShader::ALL {
        // Ensure content hashing always stays wired up for cache keys.
        assert_ne!(hash(shader), 0, "{shader:?} hash should be non-zero");
        validate_webgl2_subset(shader);
    }
}
