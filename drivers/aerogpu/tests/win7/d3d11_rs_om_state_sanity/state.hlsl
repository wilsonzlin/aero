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

// Used by the depth-clip subtest: z is set outside the canonical D3D clip volume (0 <= z <= w).
// With DepthClipEnable=TRUE this should be clipped, and with DepthClipEnable=FALSE it should render.
VSOut vs_depth_clip_main(VSIn input) {
  VSOut o;
  // z < 0 is outside the D3D clip volume when depth clipping is enabled (0 <= z <= w).
  o.pos = float4(input.pos.xy, -0.5f, 1.0f);
  o.color = input.color;
  return o;
}

float4 ps_main(VSOut input) : SV_Target {
  return input.color;
}
