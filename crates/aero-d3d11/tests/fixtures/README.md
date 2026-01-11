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
* `ps_passthrough.dxbc`
  * Shader model: `ps_4_0`
  * Chunks: `ISGN`, `OSGN`, `SHDR`
  * Behavior: `mov o0, v1` (return color), `ret`

These fixtures are **hand-authored** DXBC containers with the standard D3D10+
signature chunk layout. The SM4 token streams are the minimal subset required
by the current bootstrap translator (`mov`/`ret`).

Expected entry points (if compiled from HLSL) would be `vs_main` / `ps_main`,
but note that the DXBC blobs here are not produced by `fxc` directly.

## ILAY input-layout blobs

The AeroGPU guest↔host ABI defines an opaque input-layout blob with magic
`"ILAY"` (`AEROGPU_INPUT_LAYOUT_BLOB_MAGIC`) used by `CREATE_INPUT_LAYOUT`.

* `ilay_pos3_color.bin`: `POSITION0` (`R32G32B32_FLOAT`) + `COLOR0` (`R32G32B32A32_FLOAT`)
* `ilay_pos3_tex2.bin`: `POSITION0` (`R32G32B32_FLOAT`) + `TEXCOORD0` (`R32G32_FLOAT`)

Semantic names are represented as a 32-bit FNV-1a hash of the ASCII name, per
`drivers/aerogpu/protocol/aerogpu_cmd.h`.

## Command stream

`cmd_triangle_sm4.bin` is a minimal AeroGPU command stream (byte-packed per
`drivers/aerogpu/protocol/aerogpu_cmd.h`) that:

1. Creates a vertex buffer + index buffer and uploads a single triangle
2. Creates a render-target texture
3. Creates SM4 vertex/pixel shaders from the DXBC fixtures
4. Creates an input layout from `ilay_pos3_color.bin`
5. Binds state, draws, and emits `PRESENT`

This stream is intended for executor-style tests (e.g. “parse and replay a
captured D3D10/11 submission”) without requiring a full guest driver.

