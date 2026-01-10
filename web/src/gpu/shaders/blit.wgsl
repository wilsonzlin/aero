struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
  let positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );

  let pos = positions[vid];

  var out: VsOut;
  out.pos = vec4<f32>(pos, 0.0, 1.0);
  // WebGPU's texture coordinate origin is top-left, unlike WebGL/OpenGL.
  // Flip Y so the shader matches the WebGL presenter output for a
  // top-left-origin RGBA8 framebuffer.
  out.uv = vec2<f32>((pos.x + 1.0) * 0.5, 1.0 - (pos.y + 1.0) * 0.5);
  return out;
}

@group(0) @binding(0) var frameSampler: sampler;
@group(0) @binding(1) var frameTexture: texture_2d<f32>;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  return textureSample(frameTexture, frameSampler, in.uv);
}
