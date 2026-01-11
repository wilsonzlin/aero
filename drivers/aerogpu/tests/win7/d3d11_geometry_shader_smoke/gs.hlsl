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
  float4 color : TEXCOORD0;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  o.color = input.color;
  return o;
}

[maxvertexcount(3)]
void gs_main(triangle VSOut input[3], inout TriangleStream<GSOut> tri_stream) {
  GSOut o;
  o.pos = input[0].pos;
  o.color = input[0].color;
  tri_stream.Append(o);
  o.pos = input[1].pos;
  o.color = input[1].color;
  tri_stream.Append(o);
  o.pos = input[2].pos;
  o.color = input[2].color;
  tri_stream.Append(o);
  tri_stream.RestartStrip();
}

float4 ps_main(GSOut input) : SV_Target {
  return input.color;
}

