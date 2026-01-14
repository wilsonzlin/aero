# Win7 D3D9 UMD: fixed-function WVP transforms (`D3DFVF_XYZ*`) + `ProcessVertices`

This document summarizes how the AeroGPU Windows 7 D3D9Ex UMD applies **world/view/projection (WVP)** transforms for the
fixed-function fallback path, and how that relates to the `pfnProcessVertices` CPU transform subset.

It is referenced by:

- `docs/graphics/README.md`
- `drivers/aerogpu/umd/d3d9/README.md` (“Fixed-function vertex formats (FVF)” → “Limitations (bring-up)”)

## Draw-time WVP for fixed-function `D3DFVF_XYZ*` draws

When the D3D9 runtime is using the fixed-function fallback path with an untransformed position FVF (`D3DFVF_XYZ*`), the UMD applies WVP in one of two ways depending on the exact FVF:

### CPU clip-space conversion (XYZ|DIFFUSE{,TEX1})

For `D3DFVF_XYZ | D3DFVF_DIFFUSE{ | D3DFVF_TEX1}`, the draw paths apply the combined `WORLD0 * VIEW * PROJECTION` transform on the **CPU** and upload a clip-space copy of the referenced vertices into a scratch UP vertex buffer:

- CPU conversion helper: `convert_xyz_to_clipspace_locked()` (`drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_driver.cpp`)
- Used by: `DrawPrimitive*`, `DrawPrimitiveUP`, `DrawIndexedPrimitive*`, `DrawIndexedPrimitiveUP` fixed-function branches.
- The converted vertex data is drawn using a passthrough VS (so the GPU sees clip-space positions directly).

### VS WVP constants (XYZ|TEX1, no diffuse)

For `D3DFVF_XYZ | D3DFVF_TEX1` (no diffuse), the fixed-function fallback uses an internal VS that multiplies the input position by a WVP matrix sourced from a reserved high VS constant register range:

- Shader: `fixedfunc::kVsTransformPosWhiteTex1` (`drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_fixedfunc_shaders.h`)
- Computes `WORLD0 * VIEW * PROJECTION` from cached `Device::transform_matrices[...]` and uploads it via `ensure_fixedfunc_wvp_constants_locked()`.
- Constant range: `c240..c243` (`kFixedfuncMatrixStartRegister = 240`)
  - The constants are intentionally placed in a high register range so they are unlikely to collide with app/user shader constants when switching between fixed-function and programmable paths.
  - The cached matrices are row-major; the upload transposes to column vectors so `dp4(v, cN)` computes row-vector multiplication.
- Uploads are lazy and gated by `Device::fixedfunc_matrix_dirty`.
- Upload path: `ensure_fixedfunc_wvp_constants_locked()` + `emit_set_shader_constants_f_locked()` emitting `AEROGPU_CMD_SET_SHADER_CONSTANTS_F`.

This covers the fixed-function FVFs:

- `D3DFVF_XYZ | D3DFVF_DIFFUSE` (CPU clip-space conversion)
- `D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1` (CPU clip-space conversion)
- `D3DFVF_XYZ | D3DFVF_TEX1` (VS WVP constants; driver supplies default diffuse white)

## Pre-transformed `D3DFVF_XYZRHW*` draws (no WVP)

For pre-transformed screen-space vertices (`D3DFVF_XYZRHW*` / `POSITIONT`), the UMD does **not** use WVP transforms.
Instead, it converts `XYZRHW` to clip-space on the CPU via `convert_xyzrhw_to_clipspace_locked()` before emitting the draw.

Conversion details (bring-up):

- Uses the current D3D9 viewport (`X`, `Y`, `Width`, `Height`) and inverts the D3D9 `-0.5` pixel center convention
  (`x/y` are treated as pixel-center coordinates).
- Uses `w = 1/rhw` (with a safe fallback when `rhw==0`) and writes `clip.xyzw = {ndc.x*w, ndc.y*w, z*w, w}`.

This is used by the fixed-function FVFs:

- `D3DFVF_XYZRHW | D3DFVF_DIFFUSE{ | D3DFVF_TEX1}`
- `D3DFVF_XYZRHW | D3DFVF_TEX1` (driver supplies default diffuse white)

## `pfnProcessVertices` fixed-function CPU transform subset

Independently of draw-time WVP, `pfnProcessVertices` has a bring-up fixed-function subset in `device_process_vertices_internal()`:

- Condition: no user **vertex** shader is bound (pixel shader binding does not affect `ProcessVertices`).
- Supported source `dev->fvf` values:
  - `D3DFVF_XYZRHW` (+ optional `D3DFVF_DIFFUSE`, + optional `D3DFVF_TEX1`)
  - `D3DFVF_XYZ` (+ optional `D3DFVF_DIFFUSE`, + optional `D3DFVF_TEX1`)
- It writes screen-space `XYZRHW` into **stream 0** of the destination layout described by `hVertexDecl`. Declaration
  elements in other streams are ignored when inferring destination stride/offsets.
  - For `D3DFVF_XYZ*` inputs, it computes `WORLD0 * VIEW * PROJECTION`, applies the D3D9 viewport transform, and writes
    `XYZRHW` (screen space).
  - For `D3DFVF_XYZRHW*` inputs, `XYZRHW` is already in screen space and is passed through unchanged.
  - `D3DPV_DONOTCOPYDATA` (`ProcessVertices.Flags & 0x1`) controls whether non-position elements are written:
    - When set, the UMD writes **only** the output position (`POSITIONT` float4) and preserves all other destination bytes
      (no zeroing, no `DIFFUSE`/`TEXCOORD0` writes).
    - When not set, the UMD clears the full destination vertex before writing outputs so any elements not written become 0
      (deterministic output for extra decl fields).
    - When not set and the destination declaration includes `DIFFUSE`, the UMD copies it from the source when present,
      otherwise fills it with opaque white (matching fixed-function behavior). `TEXCOORD0` is copied only when present in
      both the source and destination layouts.

## Code anchors

- Fixed-function shader binding:
  - `ensure_fixedfunc_pipeline_locked()` (`drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_driver.cpp`)
- Draw-time fixed-function CPU conversions:
  - `convert_xyz_to_clipspace_locked()` (`XYZ` → clip-space for fixed-function `D3DFVF_XYZ | DIFFUSE{,TEX1}` draws)
  - `convert_xyzrhw_to_clipspace_locked()` (`XYZRHW`/`POSITIONT` → clip-space for pre-transformed fixed-function draws)
- Draw-time fixed-function constant upload (only for `D3DFVF_XYZ | D3DFVF_TEX1` fixed-function path):
  - `ensure_fixedfunc_wvp_constants_locked()` + `emit_set_shader_constants_f_locked()` (`AEROGPU_CMD_SET_SHADER_CONSTANTS_F`)
- Existing CPU vertex processing:
  - `device_process_vertices_internal()` (`ProcessVertices` fixed-function subset: XYZ→XYZRHW transform, XYZRHW pass-through, optional diffuse fill, honors `D3DPV_DONOTCOPYDATA`)
  - `device_process_vertices()` (DDI entrypoint; falls back to a stride-aware memcpy and clamps pre-transformed `XYZRHW*` copies to 16 bytes when `D3DPV_DONOTCOPYDATA` is set)
- Transform state cache:
  - `Device::transform_matrices[...]` (populated by `Device::SetTransform` / state blocks)
