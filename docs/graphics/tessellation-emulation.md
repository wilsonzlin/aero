# Tessellation Emulation (D3D11 HS/DS → WebGPU)

WebGPU does **not** expose:

- a **hull shader (HS)** / **domain shader (DS)** programmable tessellation pipeline, or
- the **fixed-function tessellator** that turns patches into triangles/lines.

To support D3D11 tessellation, Aero emulates HS/DS and the tessellator using a **multi-pass compute
pipeline** that produces vertex/index buffers, then draws them with a normal WebGPU render pipeline.

This document describes the intended architecture for contributors: **pass sequence**, **buffer
layouts**, the **bind group + ABI model**, the initial **supported subset**, and the **testing
strategy**.

> Related:
> - [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md) (high-level D3D11→WebGPU mapping)
> - [`docs/graphics/geometry-shader-emulation.md`](./geometry-shader-emulation.md) (compute expansion pattern + indirect draw)

---

## Why emulation is required

D3D11’s graphics pipeline (simplified) can include tessellation:

```
IA -> VS -> HS -> Tessellator (fixed-function) -> DS -> (GS) -> Rasterizer -> PS
```

WebGPU render pipelines only have:

```
Vertex -> Fragment -> OM
```

There is no programmable HS/DS stage, and there is no fixed-function tessellator. Therefore, when a
D3D11 draw uses a patchlist topology or binds an HS/DS, Aero must:

1. **Execute the missing stages in compute** (VS/HS/DS compiled to WGSL compute entry points).
2. **Explicitly materialize tessellated geometry** into buffers (storage → vertex/index).
3. **Render** the generated buffers with a small passthrough vertex shader and the original pixel
   shader.

This mirrors the approach used for geometry shader emulation: compute expansion + indirect render.

---

## Pipeline sequence (per draw)

At a high level, tessellation emulation is a fixed sequence of compute passes followed by a render
pass:

```
VS (compute) ->
  HS control-point phase (compute) ->
  HS patch-constant phase (compute) ->
  tessellator layout (compute) ->
  DS evaluation (compute) ->
  index generation (compute) ->
render (passthrough VS + original PS)
```

If a GS is bound, an additional **GS-as-compute** expansion pass runs after DS and before render
(see the GS doc).

### 0) Input assembler: patchlists

Tessellation draws come from the D3D11 IA stage with a **patchlist** topology:

- `PatchListN` means **N control points per patch** (`N ∈ [1, 32]`).
- The patch count is:
  - non-indexed: `patch_count = vertex_count / N`
  - indexed: `patch_count = index_count / N`

In native D3D11, VS runs per control point, HS runs per patch (and per output control point), and
DS runs per generated domain point.

### 1) VS-as-compute (vertex pulling)

Compute shaders cannot consume WebGPU’s vertex input interface. For any draw that routes through
compute expansion (GS/HS/DS), Aero runs the D3D VS as a compute kernel using **vertex pulling**:

- Manually load vertex attributes from the bound IA vertex buffers / index buffer.
- Execute the translated VS logic.
- Write VS outputs into a storage buffer (`vs_out`).

Implementation reference: `crates/aero-d3d11/src/runtime/vertex_pulling.rs`.

### 2) HS control-point phase (HS CP)

D3D11 hull shaders have a control-point phase that runs once for each **output control point**:

- Input: the patch’s input control points (from `vs_out`).
- Output: the patch’s output control points (`hs_cp_out`).
- System values: at minimum `SV_OutputControlPointID`, `SV_PrimitiveID`.

In emulation this is a compute pass with `patch_count * hs_output_cp_count` invocations.

### 3) HS patch-constant phase (HS PC)

Hull shaders also have a patch-constant function that runs **once per patch**:

- Inputs: the patch’s control points (VS outputs and/or HS CP outputs).
- Outputs:
  - **tess factors** (`SV_TessFactor[]`, `SV_InsideTessFactor[]`)
  - user patch constants (arbitrary HS `patchconstant` output struct)
- System values: `SV_PrimitiveID`.

In emulation this is a compute pass with `patch_count` invocations.

### 4) Tessellator layout (fixed-function emulation)

The native D3D11 tessellator is fixed-function hardware that:

1. Reads tess factors produced by HS PC.
2. Chooses a tessellation pattern based on `domain` + `partitioning` + output topology.
3. Produces a list of **domain points** (`SV_DomainLocation`) and a connectivity pattern
   (triangles/lines).

In Aero, this is a compute pass that produces metadata needed by DS + index generation:

- Per-patch **clamped tess factors** (after rounding rules).
- Per-patch **output counts** (how many domain points and how many triangles/indices).
- Per-patch **base offsets** into the global DS-vertex and index buffers (see “Offsets” below).
- Optionally, an explicit list of generated domain points, or an implicit encoding that DS can
  reconstruct deterministically.

### 5) DS evaluation (domain shader as compute)

The D3D11 domain shader runs once per generated domain point:

- Inputs:
  - HS output control points
  - HS patch constants
  - `SV_DomainLocation` (for `domain("tri")`: barycentric `(u,v,w)` with `u+v+w=1`)
  - `SV_PrimitiveID`
- Output: a vertex suitable for rasterization (position + varyings).

In Aero, DS is executed as compute and writes to a storage buffer that is also used as the final
vertex buffer for the render pass.

### 6) Index generation

WebGPU rasterization consumes triangles through either:

- non-indexed draws (`draw_indirect`), or
- indexed draws (`draw_indexed_indirect`).

For tessellation, indexed rendering is preferred because the tessellator generates a regular mesh
with shared vertices.

The index generation pass:

- emits a triangle list index buffer for each patch, and
- writes the final **indirect draw args** struct so the subsequent render pass can draw without CPU
  readback.

### 7) Render pass

Finally, the draw is rendered with a normal WebGPU render pipeline:

- Vertex stage: a small **passthrough** vertex shader that loads the generated vertex struct and
  writes the expected `@builtin(position)` + `@location` varyings.
- Fragment stage: the translated D3D pixel shader.
- Draw call: `draw_indexed_indirect` (or `draw_indirect`) using the args written by the compute
  expansion passes.

---

## Buffer layouts

Tessellation emulation relies on a set of per-draw scratch buffers. The runtime allocates them
either as separate `wgpu::Buffer`s or (preferably) as slices of a transient arena:

Implementation reference: `crates/aero-d3d11/src/runtime/expansion_scratch.rs`.

### Register-based `vec4` storage (DXBC-friendly layout)

DXBC is register-based. To avoid having to exactly reproduce HLSL packing rules (and to keep
byte-for-byte compatibility with register addressing), Aero represents shader-visible structured
data as arrays of 16-byte “registers”:

```wgsl
// Conceptual: 16-byte registers.
struct RegFile {
    regs: array<vec4<u32>, N>;
}
```

This model is used for:

- D3D constant buffers (`cb#`) (as `var<uniform>`),
- per-stage I/O stored in scratch buffers (as `var<storage>`),
- HS patch constant data.

Typed access is done via `bitcast`:

```wgsl
fn load_f32(r: vec4<u32>, lane: u32) -> f32 {
    return bitcast<f32>(r[lane]);
}
```

### Scratch buffers (logical)

Conceptually, tessellation emulation needs:

| Buffer | Usage | Produced by | Consumed by |
|---|---|---|---|
| `vs_out` | `STORAGE` | VS compute | HS CP/PC |
| `hs_cp_out` | `STORAGE` | HS CP | HS PC + DS |
| `hs_pc_out` | `STORAGE` | HS PC | tess layout + DS |
| `tess_meta` | `STORAGE` | tess layout | DS + index gen |
| `ds_vertices` | `STORAGE \| VERTEX` | DS | render (or GS) |
| `ds_indices` | `STORAGE \| INDEX` | index gen | render |
| `indirect_args` | `STORAGE \| INDIRECT` | index gen (or final stage) | render |
| `counters` | `STORAGE` (atomics) | multiple | multiple |

Notes:

- The executor already has a shared scratch allocator that supports vertex/index/indirect output
  (`ExpansionScratchAllocator`).
- Final vertex/index buffers must include `VERTEX`/`INDEX` usage because the render pass binds them
  as vertex/index buffers, not just storage.

### Offsets within scratch allocations

When scratch is sub-allocated from a larger arena, each logical buffer is represented as:

- a `wgpu::Buffer` handle (shared backing), plus
- a base `offset` (bytes) and `size` (bytes) within that buffer.

These offsets must satisfy WebGPU alignment requirements (`COPY_BUFFER_ALIGNMENT`, and if using
dynamic offsets, `min_storage_buffer_offset_alignment`).

### Per-patch offsets (vertex/index base)

Because WebGPU cannot allocate buffers dynamically on the GPU, tessellation must write into
pre-allocated output buffers. To allow each patch to write a variable amount of output, the tess
layout pass establishes per-patch offsets.

Two common strategies are valid; the intended design is to support either:

1. **Prefix-sum (deterministic)**
   - HS PC produces per-patch vertex/index counts.
   - Tess layout computes a prefix sum to assign `(base_vertex, base_index)` per patch.
2. **Atomic allocation (simple)**
   - Each patch uses `atomicAdd` on global counters to reserve a contiguous range.
   - Stores the returned base offsets in `tess_meta`.

In both cases, downstream DS and index generation use `tess_meta[patch_id]` to compute the final
write addresses.

### Indirect draw args layout

The indirect args buffer stores one of the WebGPU-defined structs at offset 0:

- `DrawIndirectArgs` (16 bytes) for `draw_indirect`, or
- `DrawIndexedIndirectArgs` (20 bytes) for `draw_indexed_indirect`.

Implementation reference: `crates/aero-d3d11/src/runtime/indirect_args.rs`.

---

## Bind group model

### Stage-scoped bind groups

Aero’s DXBC→WGSL translation and command-stream executor share a stable binding model:

- `@group(0)`: VS resources
- `@group(1)`: PS resources
- `@group(2)`: CS resources

Reference: `crates/aero-d3d11/src/binding_model.rs`.

### Extended stages (GS/HS/DS) and `stage_ex`

The AeroGPU command stream has legacy `shader_stage` enums that mirror WebGPU (VS/PS/CS) and also
includes an explicit Geometry stage (`shader_stage = GEOMETRY`).

To support additional D3D programmable stages (HS/DS) without breaking the ABI, some
resource-binding packets overload a reserved field with a small **`stage_ex` tag** when
`shader_stage == COMPUTE` (values match DXBC program types):

- Preferred GS encoding: `shader_stage = GEOMETRY`, `reserved0 = 0`.
- `stage_ex` encoding (required for HS/DS; may also be used for GS for compatibility):
  - `2 = Geometry`
  - `3 = Hull`
  - `4 = Domain`

On the host, these are tracked as distinct per-stage binding tables so that “real compute” state is
not overwritten by graphics emulation state.

Implementation references:

- `emulator/protocol/aerogpu/aerogpu_cmd.rs` (`AerogpuShaderStageEx`, `encode_stage_ex`, `decode_stage_ex`)
- `crates/aero-d3d11/src/runtime/bindings.rs` (`ShaderStage::from_aerogpu_u32_with_stage_ex`)

### Where HS/DS resources live in WGSL

Even though HS/DS run as compute, they keep the normal D3D register model (`b#`, `t#`, `s#`, `u#`).
To stay within WebGPU’s minimum `maxBindGroups == 4`, Aero routes all extended stages into the
fourth group:

- HS/DS/GS resources: `@group(3)`
- Internal expansion scratch bindings: also `@group(3)`, but in a reserved binding-number range to
  avoid collisions with D3D register bindings.

The group index mapping is encoded in `ShaderStage::as_bind_group_index()`:

- VS → 0
- PS → 1
- CS → 2
- GS/HS/DS → 3

Because `@group(3)` is shared by multiple “logical things”, the important invariant is:

- **For a given compute pipeline**, the runtime and the translated WGSL must agree on which
  `@binding` numbers correspond to:
  - D3D-style HS/DS resources (`b#/t#/s#/u#`), and
  - internal buffers (`vs_out`, `hs_cp_out`, `tess_meta`, `ds_vertices`, indices, counters, args).

---

## Supported subset (initial) and known limitations

Initial target subset:

- **Domain:** `tri`
- **Partitioning:** `integer`
- **Tess factor clamp:** clamp and round tess factors to the D3D11-valid range (max tess factor is
  **64**) and apply an additional runtime clamp if needed to keep scratch allocations bounded.

Expected limitations / non-goals for the first implementation:

- **No quad or isoline domains** (`domain("quad")` / `domain("isoline")`).
- **No fractional partitioning modes** (`fractional_even`, `fractional_odd`, `pow2`).
- **Edge-factor mismatch behavior may be incomplete**:
  - native D3D tessellation has precise crack-free rules when neighboring patches share edges;
    initial emulation may require “reasonable” factor usage (e.g. matching outer factors on shared
    edges) or may approximate by using a single effective tess factor per patch.
- **Index format:** initial implementation may emit `u32` indices unconditionally (simpler); `u16`
  can be added later when safe.
- **Backend support:** WebGL2 has no compute shaders; tessellation emulation is WebGPU-only unless a
  separate CPU fallback is added.

---

## Testing strategy

Tessellation emulation has two orthogonal correctness surfaces:

1. **The tessellator layout algorithm** (domain point generation + triangle connectivity).
2. **The end-to-end pipeline plumbing** (bindings, scratch offsets, indirect args, stage linking).

Recommended test coverage:

### Unit tests (CPU-side, deterministic)

- Implement a small, pure-Rust reference tessellator for the supported subset (tri + integer).
- Unit test:
  - tess factor clamping/rounding rules,
  - vertex/index counts for small factors (1, 2, 3…),
  - a few exact domain point sets / connectivity patterns.

These tests should run in `cargo test` without requiring a GPU.

### Pixel-compare tests (GPU integration)

Add `aero-d3d11` tests that:

1. Upload minimal VS/HS/DS/PS DXBC fixtures.
2. Bind a patchlist topology.
3. Execute a draw through the tessellation emulation path.
4. Render to a small offscreen RT and read back pixels.
5. Compare against a reference image (or a generated expected pattern with tolerances).

Suggested fixtures and tests (naming pattern aligns with existing docs/tests):

- Fixtures: `crates/aero-d3d11/tests/fixtures/{hs,ds}_*.dxbc`
- Tests: `crates/aero-d3d11/tests/aerogpu_cmd_tessellation_*.rs`

For early bring-up, it’s also useful to add a “buffer readback” test that validates:

- `indirect_args` matches expected vertex/index counts, and
- index buffer contents follow the expected topology,

before relying on rasterization correctness.
