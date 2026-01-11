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

out vec4 outColor;

void main() {
  vec4 color = texture(u_frame, v_uv);

  if (u_cursor_enable != 0 && u_cursor_size.x > 0 && u_cursor_size.y > 0) {
    // v_uv has a bottom-left origin (OpenGL/WebGL), but Aero cursor coordinates are top-left
    // to match D3D/Windows. Flip Y for cursor math.
    ivec2 srcSize = max(u_src_size, ivec2(1, 1));
    vec2 uv_tl = vec2(v_uv.x, 1.0 - v_uv.y);
    ivec2 screenPx = ivec2(uv_tl * vec2(srcSize));
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

  outColor = color;
}
