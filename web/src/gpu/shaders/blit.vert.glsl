#version 300 es

precision highp float;

out vec2 v_uv;

void main() {
  vec2 pos;
  if (gl_VertexID == 0) {
    pos = vec2(-1.0, -1.0);
  } else if (gl_VertexID == 1) {
    pos = vec2(3.0, -1.0);
  } else {
    pos = vec2(-1.0, 3.0);
  }

  // Convert clip-space to [0,1] UVs. Note that for the oversized fullscreen
  // triangle, UVs will extend beyond [0,1] but the sampler is clamped.
  // We intentionally use a top-left origin so CPU-produced framebuffers laid
  // out in row-major order (top scanline first) appear upright without relying
  // on `UNPACK_FLIP_Y_WEBGL`.
  vec2 uv = (pos + vec2(1.0, 1.0)) * 0.5;
  v_uv = vec2(uv.x, 1.0 - uv.y);
  gl_Position = vec4(pos, 0.0, 1.0);
}
