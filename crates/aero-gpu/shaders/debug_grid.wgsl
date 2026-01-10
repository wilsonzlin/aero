struct ViewportTransform {
  // Clip-space transform (scale, offset). Negative scale.y can be used to flip Y.
  scale: vec2<f32>,
  offset: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> viewport: ViewportTransform;

struct VsOut {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
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

  var uvs = array<vec2<f32>, 6>(
    vec2<f32>(0.0, 0.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, 0.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, 1.0),
  );

  let base_pos = positions[vertex_index];
  let pos = base_pos * viewport.scale + viewport.offset;

  var out: VsOut;
  out.position = vec4<f32>(pos, 0.0, 1.0);
  out.uv = uvs[vertex_index];
  return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  // 10x10 UV grid overlay (transparent background).
  let grid_uv = in.uv * 10.0;
  let grid = abs(fract(grid_uv - 0.5) - 0.5) / fwidth(grid_uv);
  let line = 1.0 - clamp(min(grid.x, grid.y), 0.0, 1.0);
  let bg = vec4<f32>(0.0, 0.0, 0.0, 0.0);
  let fg = vec4<f32>(1.0, 1.0, 1.0, 0.75);
  return mix(bg, fg, line);
}
