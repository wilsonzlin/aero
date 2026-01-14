# Win7 D3D9 UMD task: draw-time fixed-function WVP transforms (untextured `D3DFVF_XYZ | D3DFVF_DIFFUSE`)

This document tracks a **remaining gap** in the AeroGPU Windows 7 D3D9Ex UMD fixed-function bring-up path:
applying **world/view/projection (WVP)** transforms for *untextured* fixed-function draws using
`D3DFVF_XYZ | D3DFVF_DIFFUSE`.

It is referenced by the UMD README:

- `drivers/aerogpu/umd/d3d9/README.md` (“Fixed-function vertex formats (FVF)” → “Limitations (bring-up)”)

## Current behavior (as of `src/aerogpu_d3d9_driver.cpp`)

- **Fixed-function draw path** (`ensure_fixedfunc_pipeline_locked()`):
  - Binds small, precompiled fixed-function shaders.
  - For `D3DFVF_XYZ | D3DFVF_DIFFUSE`, the vertex shader path is currently **passthrough** (no draw-time WVP).
    `XYZ` positions are treated as already in clip-space.
  - For textured `XYZ` fixed-function variants (for example `D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1`), the fixed-function
    vertex shader applies the combined WVP matrix. The UMD uploads WVP into a reserved VS constant range via
    `ensure_fixedfunc_wvp_constants_locked()`.

- **Fixed-function pre-transformed input** (`D3DFVF_XYZRHW*`):
  - Before drawing, the UMD converts screen-space `XYZRHW` (`POSITIONT`) to clip-space on the CPU via
    `convert_xyzrhw_to_clipspace_locked()`.

- **`pfnProcessVertices` fixed-function subset**:
  - `device_process_vertices_internal()` implements a minimal CPU vertex pipeline for (when no user **vertex** shader is
    bound; pixel shader binding does not affect `ProcessVertices`):
    - `D3DFVF_XYZ | D3DFVF_DIFFUSE{ | D3DFVF_TEX1}`
    - `D3DFVF_XYZ | D3DFVF_TEX1`
  - It computes `WORLD0 * VIEW * PROJECTION`, then applies the D3D9 viewport transform and writes screen-space
    `XYZRHW` to the destination vertex buffer.

## Why this gap matters

Apps that rely on classic fixed-function `SetTransform` + **untextured** `D3DFVF_XYZ | D3DFVF_DIFFUSE` draws (without
explicitly calling `ProcessVertices`) will render incorrectly until draw-time WVP transforms are implemented for this
FVF variant.

This also makes Win7 bring-up/debugging harder when investigating which fixed-function path the runtime is choosing:
draw-time fixed-function vs `ProcessVertices`-assisted paths.

## Goal

When the UMD is executing the fixed-function pipeline for `D3DFVF_XYZ | D3DFVF_DIFFUSE`, it should apply the cached
transform state (`Device::transform_matrices`) so that vertices are transformed into clip-space (and clipped consistently)
without requiring `pfnProcessVertices`.

## Suggested implementation approaches

### Option A: Reuse the existing “fixed-function WVP constants” machinery

The UMD already has a draw-time WVP path for textured fixed-function `XYZ` variants:

- Vertex shader variants that multiply `POSITION` by WVP (see fixed-function shader blobs referenced by
  `ensure_fixedfunc_pipeline_locked()`).
- A reserved constant register range (`kFixedfuncMatrixStartRegister` / `kFixedfuncMatrixVec4Count`) populated by
  `ensure_fixedfunc_wvp_constants_locked()` (emitted via `AEROGPU_CMD_SET_SHADER_CONSTANTS_F`).

To close the remaining gap, extend the same approach to the untextured `D3DFVF_XYZ | D3DFVF_DIFFUSE` case by:

- adding a WVP-capable VS variant without texture coordinates (e.g. `fixedfunc::kVsWvpPosColor`), and
- selecting it in `ensure_fixedfunc_pipeline_locked()` for `kSupportedFvfXyzDiffuse` (instead of the passthrough VS),
  while continuing to use the existing fixed-function constant upload path.

### Option B: CPU-side conversion at draw time (like `XYZRHW` conversion)

- For fixed-function `D3DFVF_XYZ*` draws, map/read stream-0 vertices and CPU-transform them into a scratch buffer
  before uploading/drawing.
- This avoids shader constant management, but may be more expensive and requires robust mapping for guest-backed VBs.

## Code anchors

- Fixed-function shader binding:
  - `ensure_fixedfunc_pipeline_locked()` (`drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_driver.cpp`)
- Existing CPU vertex processing:
  - `device_process_vertices_internal()` (WVP + viewport, writes `XYZRHW`)
  - `convert_xyzrhw_to_clipspace_locked()` (`XYZRHW` → clip-space)
- Transform state cache:
  - `Device::transform_matrices[...]` (populated by `Device::SetTransform` / state blocks)
- Draw-time fixed-function constant upload:
  - `ensure_fixedfunc_wvp_constants_locked()` + `emit_set_shader_constants_f_locked()`
  - `AEROGPU_CMD_SET_SHADER_CONSTANTS_F`
