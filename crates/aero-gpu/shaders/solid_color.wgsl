struct ViewportTransform {
  // Clip-space transform (scale, offset). Negative scale.y can be used to flip Y.
  scale: vec2<f32>,
  offset: vec2<f32>,
};

struct SolidColorParams {
  color: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> viewport: ViewportTransform;

@group(0) @binding(1)
var<uniform> params: SolidColorParams;

struct VsOut {
  @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
  var positions = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0,  1.0),
  );

  let base_pos = positions[vertex_index];
  let pos = base_pos * viewport.scale + viewport.offset;

  var out: VsOut;
  out.position = vec4<f32>(pos, 0.0, 1.0);
  return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
  return params.color;
}
