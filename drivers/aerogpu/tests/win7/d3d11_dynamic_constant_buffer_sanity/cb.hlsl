cbuffer Cb0 : register(b0) {
  float4 color;
};

struct VSIn {
  float2 pos : POSITION;
};

struct VSOut {
  float4 pos : SV_Position;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  return o;
}

float4 ps_main(VSOut input) : SV_Target {
  return color;
}

