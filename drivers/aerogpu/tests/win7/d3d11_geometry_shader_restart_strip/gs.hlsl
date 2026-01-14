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

[maxvertexcount(8)]
void gs_main(point VSOut input[1], inout TriangleStream<GSOut> tri_stream) {
  float2 half = float2(0.3f, 0.3f);
  float4 bases[2] = {
      float4(-0.6f, 0.0f, 0.0f, 1.0f),
      float4(0.6f, 0.0f, 0.0f, 1.0f)
  };
  float4 colors[2] = {
      float4(0.0f, 1.0f, 0.0f, 1.0f), // green
      float4(0.0f, 0.0f, 1.0f, 1.0f)  // blue
  };

  GSOut o;
  for (int i = 0; i < 2; ++i) {
    float4 base = bases[i];
    o.color = colors[i];

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
}

float4 ps_main(GSOut input) : SV_Target {
  return input.color;
}
