Texture2D tex0 : register(t0);
SamplerState samp0 : register(s0);

struct VSIn {
  float2 pos : POSITION;
  float2 uv : TEXCOORD0;
};

struct VSOut {
  float4 pos : SV_Position;
  float2 uv : TEXCOORD0;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  o.uv = input.uv;
  return o;
}

float4 ps_main(VSOut input) : SV_Target {
  return tex0.Sample(samp0, input.uv);
}

