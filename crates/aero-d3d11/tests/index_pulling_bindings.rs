use aero_d3d11::binding_model::BINDING_BASE_INTERNAL;
use aero_d3d11::input_layout::MAX_WGPU_VERTEX_BUFFERS;
use aero_d3d11::runtime::index_pulling::{
    INDEX_PULLING_BINDING_BASE, INDEX_PULLING_BUFFER_BINDING, INDEX_PULLING_PARAMS_BINDING,
};
use aero_d3d11::runtime::vertex_pulling::{
    VERTEX_PULLING_UNIFORM_BINDING, VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE,
};

#[test]
fn index_pulling_bindings_use_internal_range_and_do_not_overlap_vertex_buffers() {
    // Both vertex and index pulling share `@group(VERTEX_PULLING_GROUP)` with D3D11 extended-stage
    // resources, so they must live in the reserved internal binding range.
    const MAX_VERTEX_BUFFER_BINDING: u32 =
        VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + MAX_WGPU_VERTEX_BUFFERS - 1;
    const {
        assert!(
            VERTEX_PULLING_UNIFORM_BINDING >= BINDING_BASE_INTERNAL,
            "vertex pulling uniform binding must be in the internal binding range"
        );
        assert!(
            INDEX_PULLING_PARAMS_BINDING >= BINDING_BASE_INTERNAL,
            "index pulling bindings must be in the internal binding range"
        );

        // Index pulling bindings are placed after the maximum possible vertex-buffer bindings.
        assert!(
            INDEX_PULLING_BINDING_BASE > MAX_VERTEX_BUFFER_BINDING,
            "index pulling bindings must not overlap vertex pulling vertex-buffer bindings"
        );
        assert!(
            INDEX_PULLING_PARAMS_BINDING == INDEX_PULLING_BINDING_BASE,
            "params binding should equal binding base"
        );
        assert!(
            INDEX_PULLING_BUFFER_BINDING == INDEX_PULLING_PARAMS_BINDING + 1,
            "index buffer binding should immediately follow params binding"
        );
    }
}
