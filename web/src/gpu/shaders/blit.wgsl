struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
}

struct CursorUniforms {
  // [src_width, src_height, cursor_enable, _pad]
  src_size_enable: vec4<i32>,
  // [cursor_x, cursor_y, hot_x, hot_y]
  cursor_pos_hot: vec4<i32>,
  // [cursor_width, cursor_height, _pad, _pad]
  cursor_size_pad: vec4<i32>,
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
@group(0) @binding(2) var cursorTexture: texture_2d<f32>;
@group(0) @binding(3) var<uniform> cursor: CursorUniforms;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  var color = textureSample(frameTexture, frameSampler, in.uv);

  let cursorEnable = cursor.src_size_enable.z;
  let cursorSize = cursor.cursor_size_pad.xy;
  if (cursorEnable != 0 && cursorSize.x > 0 && cursorSize.y > 0) {
    let srcSize = max(cursor.src_size_enable.xy, vec2<i32>(1, 1));
    let uvClamped = clamp(in.uv, vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
    var screenPx = vec2<i32>(uvClamped * vec2<f32>(srcSize));
    screenPx = clamp(screenPx, vec2<i32>(0, 0), srcSize - vec2<i32>(1, 1));

    let origin = cursor.cursor_pos_hot.xy - cursor.cursor_pos_hot.zw;
    let cursorPx = screenPx - origin;
    if (cursorPx.x >= 0 && cursorPx.y >= 0 && cursorPx.x < cursorSize.x && cursorPx.y < cursorSize.y) {
      let cuv = (vec2<f32>(cursorPx) + vec2<f32>(0.5, 0.5)) / vec2<f32>(cursorSize);
      let cursorColor = textureSample(cursorTexture, frameSampler, cuv);
      let a = cursorColor.a;
      color.rgb = cursorColor.rgb * a + color.rgb * (1.0 - a);
      color.a = a + color.a * (1.0 - a);
    }
  }

  return color;
}
