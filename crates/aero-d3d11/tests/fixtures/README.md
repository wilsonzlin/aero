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
* `gs_emit_stream1.dxbc`
  * Shader model: `gs_5_0`
  * Chunks: `SHEX`
  * Behavior: `emit_stream(1)`, `ret` (minimal stream-index fixture; used to exercise SM5 stream policy)
* `gs_emit_cut.dxbc`
  * Shader model: `gs_4_0`
  * Chunks: `SHDR`
  * Behavior: `dcl_inputprimitive`, `dcl_outputtopology`, `dcl_maxvertexcount`, `mov r0, v0[0]`, `emit`, `cut`, `ret`
* `gs_point_to_triangle.dxbc`
  * Shader model: `gs_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: emits three `SV_Position` vertices (triangle strip) to form a large
    triangle covering the center of the render target.
* `hs_minimal.dxbc`
  * Shader model: `hs_5_0`
  * Chunks: `ISGN`, `OSGN`, `PCSG`, `SHEX`
  * Behavior:
    * Control-point phase: writes `SV_OutputControlPointID` + `SV_PrimitiveID` into `o0.xy`, `ret`
    * Patch-constant phase: writes `SV_TessFactor` + `SV_InsideTessFactor`, `ret`

These fixtures are **hand-authored** DXBC containers with the standard D3D10+
signature chunk layout. The SM4 token streams are intentionally tiny:

* A single “declaration-like” opcode (to exercise the SM4 decoder’s “skip decls”
  logic)
* `mov`/`ret` instructions (so both the bootstrap translator and the real SM4
  decoder/translator can consume them). The `ps_add.dxbc` fixture is an
  exception: it includes an `add` to ensure tests cover the signature-driven
  translation path.

Expected entry points (if compiled from HLSL) would be `vs_main` / `ps_main`,
but note that the DXBC blobs here are not produced by `fxc` directly.

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
```

Example compilation commands:

```bat
fxc /nologo /T vs_4_0 /E vs_main /Fo vs_passthrough.dxbc vs_passthrough.hlsl
fxc /nologo /T vs_4_0 /E vs_main /Fo vs_matrix.dxbc vs_matrix.hlsl
fxc /nologo /T ps_4_0 /E ps_main /Fo ps_passthrough.dxbc ps_passthrough.hlsl
fxc /nologo /T ps_4_0 /E ps_main /Fo ps_sample.dxbc ps_sample.hlsl
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
