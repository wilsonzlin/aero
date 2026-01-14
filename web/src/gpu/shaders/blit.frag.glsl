#version 300 es

precision highp float;
precision highp int;

in vec2 v_uv;

uniform sampler2D u_frame;
uniform sampler2D u_cursor;
uniform ivec2 u_src_size;
uniform int u_cursor_enable;
uniform ivec2 u_cursor_pos;
uniform ivec2 u_cursor_hot;
uniform ivec2 u_cursor_size;
uniform int u_force_opaque_alpha;

out vec4 outColor;

float srgbEncodeChannel(float x) {
  float v = clamp(x, 0.0, 1.0);
  if (v <= 0.0031308) return v * 12.92;
  return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

vec3 srgbEncode(vec3 rgb) {
  return vec3(
    srgbEncodeChannel(rgb.r),
    srgbEncodeChannel(rgb.g),
    srgbEncodeChannel(rgb.b)
  );
}

void main() {
  vec4 color = texture(u_frame, v_uv);

  if (u_cursor_enable != 0 && u_cursor_size.x > 0 && u_cursor_size.y > 0) {
    // `v_uv` uses a top-left origin (see `blit.vert.glsl`) to match D3D/Windows,
    // so we can use it directly for cursor math.
    ivec2 srcSize = max(u_src_size, ivec2(1, 1));
    ivec2 screenPx = ivec2(v_uv * vec2(srcSize));
    screenPx = clamp(screenPx, ivec2(0, 0), srcSize - ivec2(1, 1));

    ivec2 origin = u_cursor_pos - u_cursor_hot;
    ivec2 cursorPx = screenPx - origin;
    if (cursorPx.x >= 0 && cursorPx.y >= 0 && cursorPx.x < u_cursor_size.x && cursorPx.y < u_cursor_size.y) {
      vec2 cuv = (vec2(cursorPx) + vec2(0.5)) / vec2(u_cursor_size);
      vec4 cursorColor = texture(u_cursor, cuv);
      float a = cursorColor.a;
      color.rgb = cursorColor.rgb * a + color.rgb * (1.0 - a);
      color.a = a + color.a * (1.0 - a);
    }
  }

  // Presentation policy: output is sRGB.
  //
  // Most callers want opaque alpha (the default); diagnostics may request that we preserve
  // the source alpha channel so incorrect XRGB/BGRX handling is visible against the page
  // background.
  color.rgb = srgbEncode(color.rgb);
  if (u_force_opaque_alpha != 0) {
    color.a = 1.0;
  }
  outColor = color;
}
