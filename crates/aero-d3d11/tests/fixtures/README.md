# Aero D3D11 fixtures (SM4 DXBC + ILAY + AeroGPU cmd stream)

This directory contains **checked-in binary fixtures** used by `aero-d3d11` tests to
exercise the full “real blob” path:

`DXBC container → signature chunks → SM4 token stream → WGSL → naga parse`.

The files are intentionally tiny and deterministic, so CI does **not** require
`fxc.exe`/`dxc.exe`.

## DXBC shaders

* `vs_passthrough.dxbc`
  * Shader model: `vs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `mov o0, v0` (position), `mov o1, v1` (color), `ret`
* `vs_passthrough_texcoord.dxbc`
  * Shader model: `vs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `mov o1, v0` (position), `mov o0, v1` (texcoord), `ret`
* `vs_matrix.dxbc`
  * Shader model: `vs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `dp4 o0.{x,y,z,w}, v0, cb0[{0,1,2,3}]` (matrix multiply), `ret`
* `ps_passthrough.dxbc`
  * Shader model: `ps_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `mov o0, v1` (return color), `ret`
* `ps_primitive_id.dxbc`
  * Shader model: `ps_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `movc o0, v0, l(1,0,0,1), l(0,0,0,1)` (red for `SV_PrimitiveID != 0`), `ret`
* `ps_add.dxbc`
  * Shader model: `ps_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `add_sat o0, v1, v1` (force signature-driven translator), `ret`
* `ps_sample.dxbc`
  * Shader model: `ps_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `sample o0, v0, t0, s0`, `ret`
* `ps_ld.dxbc`
  * Shader model: `ps_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `ld o0, l(0,0,0,0), t0`, `ret`
* `ps_if_movc.dxbc`
  * Shader model: `ps_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `lt` + `movc` + `if/else/endif` to drive a simple branchy color output
* `gs_passthrough.dxbc`
  * Shader model: `gs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: triangle input → triangle strip output, `maxvertexcount=3`, `emit`×3 (passthrough)
* `gs_emit_triangle.dxbc`
  * Shader model: `gs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Decl section includes GS-specific declarations:
    * `dcl_input_primitive` (triangle)
    * `dcl_output_topology` (triangle strip)
    * `dcl_max_output_vertex_count` (3)
  * Behavior: writes a few immediate positions/colors to output registers, then
    `emit` ×3, `cut`, `ret`
* `cs_store_uav_raw.dxbc`
  * Shader model: `cs_5_0`
  * Chunks: `SHEX`
  * Behavior: `[numthreads(1,1,1)]`, `dcl_uav_raw u0`, `store_raw u0.x, 0, 0x12345678`, `ret`
* `cs_copy_raw_srv_to_uav.dxbc`
  * Shader model: `cs_5_0`
  * Chunks: `SHEX`
  * Behavior: `[numthreads(1,1,1)]`, `dcl_resource_raw t0`, `dcl_uav_raw u0`, `ld_raw r0.xyzw, 0, t0`,
    `store_raw u0.xyzw, 0, r0`, `ret`
* `cs_copy_structured_srv_to_uav.dxbc`
  * Shader model: `cs_5_0`
  * Chunks: `SHEX`
  * Behavior: `[numthreads(1,1,1)]`, `dcl_resource_structured t0, stride=16`, `dcl_uav_structured u0, stride=16`,
    `ld_structured r0.xyzw, index=1, offset=0, t0`, `store_structured u0.xyzw, index=0, offset=0, r0`, `ret`
* `cs_ld_uav_raw_float_addr.dxbc`
  * Shader model: `cs_5_0`
  * Chunks: `SHEX`
  * Behavior: `[numthreads(1,1,1)]`, `dcl_uav_raw u0`, `dcl_uav_raw u1`,
    `ld_uav_raw r0.xyzw, 16, u0`, `store_raw u1.xyzw, 0, r0`,
    `ld_uav_raw r1.xyzw, 16.0, u0`, `store_raw u1.xyzw, 16, r1`, `ret`
* `gs_emit_stream1.dxbc`
  * Shader model: `gs_5_0`
  * Chunks: `SHEX`
  * Behavior: `emit_stream(1)`, `ret` (minimal stream-index fixture; used to exercise SM5 stream policy)
* `gs_cut_stream1.dxbc`
  * Shader model: `gs_5_0`
  * Chunks: `SHEX`
  * Behavior: `cut_stream(1)`, `ret` (minimal stream-index fixture; used to exercise SM5 stream policy)
* `gs_emitthen_cut_stream1.dxbc`
  * Shader model: `gs_5_0`
  * Chunks: `SHEX`
  * Behavior: `emitthen_cut_stream(1)`, `ret` (minimal stream-index fixture; used to exercise SM5 stream policy)
* `gs_cut.dxbc`
  * Shader model: `gs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: point input → triangle strip output, `maxvertexcount=4`, emits a quad (4 verts) then `cut`, `ret` (RestartStrip semantics)
* `gs_emit_stream_cut_stream.dxbc`
  * Shader model: `gs_5_0`
  * Chunks: `SHEX`
  * Behavior: `dcl_inputprimitive`, `dcl_outputtopology`, `dcl_maxvertexcount`, `emit_stream(2)`, `cut_stream(3)`, `ret`
* `gs_point_to_triangle.dxbc`
  * Shader model: `gs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: emits three `SV_Position` vertices (triangle strip) to form a large
    triangle covering the center of the render target.
  * Note: the token stream uses `emit` (`OPCODE_EMIT = 0x3f`) and `cut`
    (`OPCODE_CUT = 0x40`) opcode IDs from `aero_d3d11::sm4::opcode` (do not
    confuse with `emitthen_cut` / `emitthen_cut_stream`, which use opcode IDs
    `0x43` / `0x44`).
* `hs_minimal.dxbc`
  * Shader model: `hs_5_0`
  * Chunks: `ISGN`, `OSGN`, `PCSG`, `SHEX`
  * Behavior:
    * Control-point phase: writes `SV_OutputControlPointID` + `SV_PrimitiveID` into `o0.xy`, `ret`
    * Patch-constant phase: writes `SV_TessFactor` + `SV_InsideTessFactor`, `ret`
* `hs_ret.dxbc`
  * Shader model: `hs_5_0`
  * Chunks: `ISGN`, `OSGN`, `PCSG`, `SHEX`
  * Behavior: `ret` only (minimal HS translation smoke test; validates HS lowering emits WGSL `@compute`)
* `hs_tri_integer.dxbc`
  * Shader model: `hs_5_0`
  * Chunks: `ISGN`, `OSGN`, `SHEX`
  * Behavior: outputs constant tess factors (`SV_TessFactor`/`SV_InsideTessFactor` = 4) and
    passes through control point positions.
* `ds_tri_passthrough.dxbc`
  * Shader model: `ds_5_0`
  * Chunks: `ISGN`, `OSGN`, `SHEX`
  * Behavior: outputs position as barycentric interpolation of control point positions and
    encodes barycentric coordinates into `COLOR0` for validation.
* `ds_ret.dxbc`
  * Shader model: `ds_5_0`
  * Chunks: `ISGN`, `OSGN`, `PSGN`, `SHEX`
  * Behavior: `ret` only (minimal DS translation smoke test; validates DS lowering emits WGSL `@compute`)

These fixtures are **hand-authored** DXBC containers with the standard D3D10+
signature chunk layout. The SM4 token streams are intentionally tiny:

* A single “declaration-like” opcode (to exercise the SM4 decoder’s “skip decls”
  logic)
* `mov`/`ret` instructions (so both the bootstrap translator and the real SM4
  decoder/translator can consume them). The `ps_add.dxbc` fixture is an
  exception: it includes an `add` to ensure tests cover the signature-driven
  translation path.

Expected entry points (if compiled from HLSL) would be `vs_main` / `ps_main` /
`gs_main` / `cs_main`, but note that the DXBC blobs here are not produced by `fxc`
directly.

Equivalent HLSL (illustrative):

```hlsl
// vs_passthrough.hlsl
struct VsIn { float3 pos : POSITION0; float4 color : COLOR0; };
struct VsOut { float4 pos : SV_Position; float4 color : COLOR0; };
VsOut vs_main(VsIn i) { VsOut o; o.pos = float4(i.pos, 1); o.color = i.color; return o; }

// vs_matrix.hlsl
cbuffer Cb0 : register(b0) { float4x4 mvp; };
float4 vs_main(float3 pos : POSITION0) : SV_Position { return mul(float4(pos, 1), mvp); }

// ps_passthrough.hlsl
float4 ps_main(float4 pos : SV_Position, float4 color : COLOR0) : SV_Target0 { return color; }

// ps_sample.hlsl
Texture2D t0 : register(t0);
SamplerState s0 : register(s0);
float4 ps_main(float2 uv : TEXCOORD0) : SV_Target0 { return t0.Sample(s0, uv); }

// gs_passthrough.hlsl
struct GsIn { float4 pos : SV_Position; };
struct GsOut { float4 pos : SV_Position; };
[maxvertexcount(3)]
void gs_main(triangle GsIn input[3], inout TriangleStream<GsOut> stream) {
  GsOut o;
  o.pos = input[0].pos; stream.Append(o);
  o.pos = input[1].pos; stream.Append(o);
  o.pos = input[2].pos; stream.Append(o);
}

// gs_cut.hlsl
struct GsIn { float4 pos : SV_Position; float4 color : COLOR0; };
struct GsOut { float4 pos : SV_Position; float4 color : COLOR0; };
[maxvertexcount(4)]
void gs_main(point GsIn input[1], inout TriangleStream<GsOut> stream) {
  GsOut o;
  o.color = input[0].color;
  o.pos = input[0].pos + float4(-0.3, -0.3, 0, 0); stream.Append(o);
  o.pos = input[0].pos + float4(-0.3,  0.3, 0, 0); stream.Append(o);
  o.pos = input[0].pos + float4( 0.3, -0.3, 0, 0); stream.Append(o);
  o.pos = input[0].pos + float4( 0.3,  0.3, 0, 0); stream.Append(o);
  stream.RestartStrip();
}

// gs_point_to_triangle.hlsl
[maxvertexcount(3)]
void gs_main(point GsIn input[1], inout TriangleStream<GsOut> stream) {
  GsOut o;
  o.pos = input[0].pos + float4(-0.5, -0.5, 0, 0); stream.Append(o);
  o.pos = input[0].pos + float4( 0.5, -0.5, 0, 0); stream.Append(o);
  o.pos = input[0].pos + float4( 0.0,  0.5, 0, 0); stream.Append(o);
}

// cs_store_uav_raw.hlsl
RWByteAddressBuffer u0 : register(u0);
[numthreads(1,1,1)]
void cs_main() { u0.Store(0, 0x12345678); }
```

Example compilation commands (for reference; the checked-in blobs are hand-authored to keep CI deterministic and do not require `fxc.exe` at test time). These commands assume `fxc.exe` from a Windows 10+ SDK (d3dcompiler_47):

```bat
fxc /nologo /T vs_4_0 /E vs_main /Fo vs_passthrough.dxbc vs_passthrough.hlsl
fxc /nologo /T vs_4_0 /E vs_main /Fo vs_matrix.dxbc vs_matrix.hlsl
fxc /nologo /T ps_4_0 /E ps_main /Fo ps_passthrough.dxbc ps_passthrough.hlsl
fxc /nologo /T ps_4_0 /E ps_main /Fo ps_sample.dxbc ps_sample.hlsl
fxc /nologo /T gs_4_0 /E gs_main /Fo gs_passthrough.dxbc gs_passthrough.hlsl
fxc /nologo /T gs_4_0 /E gs_main /Fo gs_point_to_triangle.dxbc gs_point_to_triangle.hlsl
fxc /nologo /T gs_4_0 /E gs_main /Fo gs_cut.dxbc gs_cut.hlsl
fxc /nologo /T cs_5_0 /E cs_main /Fo cs_store_uav_raw.dxbc cs_store_uav_raw.hlsl
```

## DXBC token dump (opcode discovery)

For GS opcode discovery and fixture authoring, the repo contains a developer tool that dumps the
DXBC chunk list plus the raw SM4/SM5 token stream with opcode IDs/lengths and best-effort decoded
instructions:

```bash
cargo run -p aero-d3d11 --bin dxbc_dump -- gs_cut.dxbc
```

## ILAY input-layout blobs

The AeroGPU guest↔host ABI defines an opaque input-layout blob with magic
`"ILAY"` (`AEROGPU_INPUT_LAYOUT_BLOB_MAGIC`) used by `CREATE_INPUT_LAYOUT`.

* `ilay_pos3_color.bin`: `POSITION0` (`R32G32B32_FLOAT`) + `COLOR0` (`R32G32B32A32_FLOAT`)
* `ilay_pos3_tex2.bin`: `POSITION0` (`R32G32B32_FLOAT`) + `TEXCOORD0` (`R32G32_FLOAT`)

Semantic names are represented as a 32-bit FNV-1a hash of the **ASCII uppercase**
name, per
`drivers/aerogpu/protocol/aerogpu_cmd.h`.

## Command stream

`cmd_triangle_sm4.bin` is a minimal AeroGPU command stream (byte-packed per
`drivers/aerogpu/protocol/aerogpu_cmd.h`) that:

1. Creates a vertex buffer + index buffer and uploads a fullscreen triangle
   (solid red vertex colors)
   - Note: the index buffer upload is padded to 8 bytes to satisfy
     `wgpu::COPY_BUFFER_ALIGNMENT` (4-byte alignment).
2. Creates a render-target texture
3. Creates SM4 vertex shader (`vs_passthrough.dxbc`) and pixel shader
   (`ps_add.dxbc`) from the DXBC fixtures
4. Creates an input layout from `ilay_pos3_color.bin`
5. Binds state, draws, and emits `PRESENT`

This stream is intended for executor-style tests (e.g. “parse and replay a
captured D3D10/11 submission”) without requiring a full guest driver.
