# Win7 D3D9 UMD: fixed-function WVP transforms (`D3DFVF_XYZ*`) + `ProcessVertices`

This document summarizes how the AeroGPU Windows 7 D3D9Ex UMD applies **world/view/projection (WVP)** transforms for the
fixed-function fallback path, and how that relates to the `pfnProcessVertices` CPU transform subset.

It is referenced by:

- `docs/graphics/README.md`
- `drivers/aerogpu/umd/d3d9/README.md` (“Fixed-function vertex formats (FVF)” → “Limitations (bring-up)”)

## Draw-time WVP for fixed-function `D3DFVF_XYZ*` draws

When the D3D9 runtime is using the fixed-function fallback path with an untransformed position FVF (`D3DFVF_XYZ*`), the UMD:

- Selects a fixed-function vertex shader that multiplies the input position by a WVP matrix (`ensure_fixedfunc_pipeline_locked()`).
- Computes `WORLD0 * VIEW * PROJECTION` from cached `Device::transform_matrices[...]` and uploads it into a reserved VS
  constant register range via `ensure_fixedfunc_wvp_constants_locked()`.
  - The matrix cache is row-major; the upload transposes to column vectors for the shader constant layout.
  - Uploads are lazy and gated by `Device::fixedfunc_matrix_dirty`.

This is used by the fixed-function FVFs:

- `D3DFVF_XYZ | D3DFVF_DIFFUSE`
- `D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1`
- `D3DFVF_XYZ | D3DFVF_TEX1` (driver supplies default diffuse white)

## Pre-transformed `D3DFVF_XYZRHW*` draws (no WVP)

For pre-transformed screen-space vertices (`D3DFVF_XYZRHW*` / `POSITIONT`), the UMD does **not** use WVP transforms.
Instead, it converts `XYZRHW` to clip-space on the CPU via `convert_xyzrhw_to_clipspace_locked()` before emitting the draw.

This is used by the fixed-function FVFs:

- `D3DFVF_XYZRHW | D3DFVF_DIFFUSE{ | D3DFVF_TEX1}`
- `D3DFVF_XYZRHW | D3DFVF_TEX1` (driver supplies default diffuse white)

## `pfnProcessVertices` fixed-function CPU transform subset

Independently of draw-time WVP, `pfnProcessVertices` has a bring-up fixed-function subset in `device_process_vertices_internal()`:

- Condition: no user **vertex** shader is bound (pixel shader binding does not affect `ProcessVertices`).
- Supported source `dev->fvf` values:
  - `D3DFVF_XYZRHW | D3DFVF_DIFFUSE{ | D3DFVF_TEX1}` and `D3DFVF_XYZRHW | D3DFVF_TEX1`
  - `D3DFVF_XYZ | D3DFVF_DIFFUSE{ | D3DFVF_TEX1}` and `D3DFVF_XYZ | D3DFVF_TEX1`
- It computes `WORLD0 * VIEW * PROJECTION`, applies the D3D9 viewport transform, and writes screen-space `XYZRHW` into the
  destination layout described by `hVertexDecl`.
  - For `D3DFVF_XYZRHW*` inputs, `XYZRHW` is already in screen space and is passed through unchanged.
  - When the destination declaration includes diffuse color and the source format does not, the UMD fills it with opaque
    white (matching fixed-function behavior).

## Code anchors

- Fixed-function shader binding:
  - `ensure_fixedfunc_pipeline_locked()` (`drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_driver.cpp`)
- Draw-time fixed-function constant upload:
  - `ensure_fixedfunc_wvp_constants_locked()` + `emit_set_shader_constants_f_locked()`
  - `AEROGPU_CMD_SET_SHADER_CONSTANTS_F`
- Existing CPU vertex processing:
  - `device_process_vertices_internal()` (`ProcessVertices` fixed-function subset; WVP + viewport, writes `XYZRHW`)
  - `convert_xyzrhw_to_clipspace_locked()` (`XYZRHW` → clip-space for fixed-function draws)
- Transform state cache:
  - `Device::transform_matrices[...]` (populated by `Device::SetTransform` / state blocks)
