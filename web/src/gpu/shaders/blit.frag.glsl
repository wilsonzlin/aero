#version 300 es

precision highp float;

in vec2 v_uv;

uniform sampler2D u_frame;

out vec4 outColor;

void main() {
  outColor = texture(u_frame, v_uv);
}

