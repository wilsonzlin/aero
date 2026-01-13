struct VSIn {
  float2 pos : POSITION;
  float4 color : COLOR0;
};

struct VSOut {
  float4 pos : SV_Position;
  float4 color : COLOR0;
};

struct GSOut {
  float4 pos : SV_Position;
  float4 color : COLOR0;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  o.color = input.color;
  return o;
}

[maxvertexcount(4)]
void gs_main(point VSOut input[1], inout TriangleStream<GSOut> tri_stream) {
  float4 base = input[0].pos;
  float2 half = float2(0.3f, 0.3f);

  GSOut o;
  o.color = input[0].color;

  // Triangle strip quad (4 vertices) + RestartStrip to terminate the strip.
  o.pos = base + float4(-half.x, -half.y, 0.0f, 0.0f);
  tri_stream.Append(o);
  o.pos = base + float4(-half.x,  half.y, 0.0f, 0.0f);
  tri_stream.Append(o);
  o.pos = base + float4( half.x, -half.y, 0.0f, 0.0f);
  tri_stream.Append(o);
  o.pos = base + float4( half.x,  half.y, 0.0f, 0.0f);
  tri_stream.Append(o);

  tri_stream.RestartStrip();
}

float4 ps_main(GSOut input) : SV_Target {
  return input.color;
}

