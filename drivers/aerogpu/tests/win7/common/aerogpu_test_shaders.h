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

}  // namespace aerogpu_test

