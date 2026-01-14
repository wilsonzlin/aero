#pragma once

namespace aerogpu_test {

// Simple pass-through vertex shader + solid color pixel shader used by the D3D11 tests.
static const char kAeroGpuTestBasicColorHlsl[] = R"(
struct VSIn {
  float2 pos : POSITION;
  float4 color : COLOR0;
};

struct VSOut {
  float4 pos : SV_Position;
  float4 color : COLOR0;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  o.color = input.color;
  return o;
}

float4 ps_main(VSOut input) : SV_Target {
  return input.color;
}
)";

// Constant-buffer test shader used to validate VS/PS cbuffer bindings.
//
// Expected constant buffer layout (register b0):
//   float4 vs_color;  // offset 0
//   float4 ps_mod;    // offset 16
static const char kAeroGpuTestConstantBufferColorHlsl[] = R"(
cbuffer CB0 : register(b0) {
  float4 vs_color;
  float4 ps_mod;
};

struct VSIn {
  float2 pos : POSITION;
  float4 color : COLOR0;
};

struct VSOut {
  float4 pos : SV_Position;
  float4 color : COLOR0;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  o.color = vs_color;
  // Ensure COLOR0 is retained in the VS input signature so CreateInputLayout can
  // still validate the COLOR element. The condition is expected to be false for
  // all test vertices, so this should not affect output.
  if (input.color.x < -1.0e20f) {
    o.color = input.color;
  }
  return o;
}

float4 ps_main(VSOut input) : SV_Target {
  return input.color * ps_mod;
}
)";

}  // namespace aerogpu_test
