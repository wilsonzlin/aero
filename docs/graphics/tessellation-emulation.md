# Tessellation Emulation (D3D11 HS/DS → WebGPU)

WebGPU does **not** expose hardware tessellation: there is no hull shader (HS), domain shader (DS),
or fixed-function tessellator stage. To support D3D11 tessellation (`VS → HS → Tessellator → DS`),
Aero emulates tessellation by running the missing stages as **compute kernels**, expanding patches
into explicit vertex/index buffers, and then drawing those buffers with a normal WebGPU render
pipeline.

This document describes the chosen HS/DS emulation approach so future contributors can extend it
(quads, fractional partitioning, performance). It focuses on:

- trigger conditions (when draws route through HS/DS emulation),
- pass sequencing (VS-as-compute → HS CP → HS PC → layout → DS → index → render),
- buffer layouts (register files, expanded geometry, indirect args),
- binding model decisions (`@group(3)` for extended stages, plus per-pass internal bind groups),
- limits/clamps and tuning knobs,
- and testing/fixtures.

> Current repo status (important):
>
> - The D3D11 executor already routes draws through a compute prepass when GS/HS/DS emulation is
>   required (see `gs_hs_ds_emulation_required()` in
>   `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
> - Patchlist topology **without HS/DS bound** currently runs the built-in **synthetic expansion**
>   compute prepass that expands a deterministic triangle (to validate render-pass splitting +
>   indirect draw plumbing), not real tessellation semantics.
> - Patchlist topology **with HS+DS bound** (currently PatchList3 only) routes through an initial
>   tessellation prepass pipeline (VS-as-compute vertex pulling placeholder + HS passthrough + layout
>   pass + DS passthrough). This path currently requires an input layout for vertex pulling. Guest
>   HS/DS DXBC is not executed yet; tess factors are currently fixed in the passthrough HS
>   (currently `4.0`).
> - The in-progress tessellation runtime lives under
>   `crates/aero-d3d11/src/runtime/tessellation/` and contains real building blocks (layout pass,
>   tri-domain integer index generation, DS evaluation templates, sizing/guardrails). This code is
>   now wired into the command-stream draw path for PatchList3+HS+DS bring-up, but the guest HS/DS
>   DXBC is still not executed yet (the pipeline currently uses passthrough/stub stages).
>
> See [`docs/graphics/status.md`](./status.md) for an implementation-status checklist.

> Related:
>
> - High-level D3D10/11 mapping: [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md)
> - GS compute expansion notes: [`docs/graphics/geometry-shader-emulation.md`](./geometry-shader-emulation.md)

---

## When tessellation emulation triggers

Tessellation emulation is required when **either** of these are true:

1. A **patch-list primitive topology** is selected:
   - `AEROGPU_TOPOLOGY_*_CONTROL_POINT_PATCHLIST` (values 33–64)
   - Host-side enum: `CmdPrimitiveTopology::PatchList { control_points }`
2. A **Hull Shader** and/or **Domain Shader** is bound:
   - `hs != 0` and/or `ds != 0` in the extended `AEROGPU_CMD_BIND_SHADERS` packet.

The executor currently uses a single predicate (`gs_hs_ds_emulation_required()`) that triggers the
compute-expansion path for **all** “missing WebGPU stages / topologies” cases:

- GS/HS/DS bound, **or**
- adjacency topologies, **or**
- patchlists.

Patchlist topology triggers the compute-prepass path even before full tessellation is implemented:

- Patchlist draws without HS/DS use the synthetic expansion prepass (bring-up coverage).
- Patchlist draws with HS+DS bound use the tessellation prepass pipeline (VS/HS/layout/DS passes).

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
D3D11 draw uses patchlists or binds an HS/DS, Aero must:

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
VS-as-compute ->
  HS control-point phase ->
  HS patch-constant phase ->
  tessellator layout ->
  DS evaluation ->
  index generation ->
render (passthrough VS + original PS)
```

If a GS is bound, an additional **GS-as-compute** expansion pass runs after DS and before render
(see the GS doc).

Bring-up note (what is currently wired into the executor for PatchList3+HS+DS):

- `VS-as-compute` is executed as a compute shader using vertex pulling, but it is still a
  passthrough placeholder (guest VS DXBC is not executed yet).
- The HS is currently a **passthrough** kernel that copies control points and writes a fixed tess
  factor (`4.0`) into `hs_tess_factors` (one `vec4<f32>` per patch).
- The DS is currently a **passthrough** kernel that expands triangle-domain integer tessellation and
  emits both:
  - `ExpandedVertex { pos, varyings[32] }` records, and
  - a u32 triangle-list index buffer.

The remainder of this section describes the *target* HS/DS pipeline; many passes already exist as
unit-testable building blocks, but guest HS/DS DXBC execution and full stage linking are still
in-progress.

### 0) Input assembler: patchlists

Tessellation draws come from the D3D11 IA stage with a **patchlist** topology:

- `PatchListN` means **N control points per patch** (`N ∈ [1, 32]`).
- The patch count is:
  - non-indexed: `patch_count = vertex_count / N`
  - indexed: `patch_count = index_count / N`
  - instanced: `patch_count_total = patch_count * instance_count` (the patch stream is replicated
    per instance). Many compute passes flatten `(instance_id, patch_id)` into a single
    `patch_instance_id = instance_id * patch_count + patch_id` (see
    `docs/16-d3d10-11-translation.md`).

In native D3D11, VS runs per control point, HS runs per patch (and per output control point), and
DS runs per generated domain point.

### 1) VS-as-compute (vertex pulling)

Compute shaders cannot consume WebGPU’s vertex input interface. For any draw that routes through
compute expansion (GS/HS/DS), Aero runs the D3D VS as a compute kernel using **vertex pulling**:

- Manually load vertex attributes from the bound IA vertex buffers / index buffer.
- Execute the translated VS logic (or a stub during bring-up).
- Write VS outputs into a storage buffer (`vs_out`).

Implementation references:

- shared vertex pulling ABI: `crates/aero-d3d11/src/runtime/vertex_pulling.rs`
- tessellation VS-as-compute bring-up stub: `crates/aero-d3d11/src/runtime/tessellation/vs_as_compute.rs`

### 2) HS control-point phase (HS CP)

D3D11 hull shaders have a control-point phase that runs once for each **output control point**:

- Input: the patch’s input control points (from `vs_out`).
- Output: the patch’s output control points (`hs_cp_out`).
- System values: at minimum `SV_OutputControlPointID`, `SV_PrimitiveID`.

In emulation this is a compute pass with `patch_count_total * hs_output_cp_count` invocations.

Host-side dispatch plumbing lives in `crates/aero-d3d11/src/runtime/tessellation/hull.rs`.

### 3) HS patch-constant phase (HS PC)

Hull shaders also have a patch-constant function that runs **once per patch**:

- Inputs: the patch’s control points (VS outputs and/or HS CP outputs).
- Outputs:
  - **tess factors** (`SV_TessFactor[]`, `SV_InsideTessFactor[]`)
  - user patch constants (arbitrary HS `patchconstant` output struct)
- System values: `SV_PrimitiveID`.

In emulation this is a compute pass with `patch_count_total` invocations.

### 4) Tessellator layout (fixed-function emulation)

The native D3D11 tessellator is fixed-function hardware that:

1. Reads tess factors produced by HS PC.
2. Chooses a tessellation pattern based on `domain` + `partitioning` + output topology.
3. Produces a list of **domain points** (`SV_DomainLocation`) and a connectivity pattern
   (triangles/lines).

In Aero, this is a compute pass that produces metadata needed by DS + index generation:

- Per-patch **derived tess level** and clamped tess factors (after rounding rules).
- Per-patch **output counts** (how many domain points and indices).
- Per-patch **base offsets** into the global vertex/index buffers.
- Final **indirect draw args** (so render can be indirect without CPU readback).

Current implementation note: the repo contains a deterministic prefix-sum layout pass for
triangle-domain integer tessellation in
`crates/aero-d3d11/src/runtime/tessellation/layout_pass.rs`.

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

Implementation reference: `crates/aero-d3d11/src/runtime/tessellation/domain_eval.rs`.

### 6) Index generation

WebGPU rasterization consumes triangles through either:

- non-indexed draws (`draw_indirect`), or
- indexed draws (`draw_indexed_indirect`).

For tessellation, indexed rendering is preferred because the tessellator generates a regular mesh
with shared vertices.

Current implementation note: `crates/aero-d3d11/src/runtime/tessellation/tri_domain_integer.rs`
implements a compute pass that writes a packed `u32` triangle-list index buffer for triangle-domain
integer tessellation.

Bring-up note: the currently-wired PatchList3 DS passthrough kernel emits indices directly, so the
standalone index-generation pass is not used in the executor draw path yet.

### 7) Render pass

Finally, the draw is rendered with a normal WebGPU render pipeline:

- Vertex stage: a small **passthrough** vertex shader that loads the generated vertex struct and
  writes the expected `@builtin(position)` + `@location` varyings.
- Fragment stage: the translated D3D pixel shader.
- Draw call: `draw_indexed_indirect` (or `draw_indirect`) using the args written by the compute
  expansion passes.

---

## Buffer layouts

Tessellation emulation is “buffer-first”: each stage writes explicit results into storage buffers.
The runtime sources transient allocations from the per-frame scratch allocator
`ExpansionScratchAllocator` (`crates/aero-d3d11/src/runtime/expansion_scratch.rs`) so these buffers
do not churn per draw.

### Register files (“register-file stride”)

DXBC stages communicate via **register files** (e.g. `o0..oN` outputs). When running a stage as
compute, Aero models a register file as a runtime-sized array of **16-byte registers**:

```wgsl
// One register = 16 bytes (4x u32) so float/int/bool bit patterns are preserved.
struct AeroRegFile {
  regs: array<vec4<u32>>,
};
```

Each invocation (control point, patch, or domain point) owns a contiguous slice of `regs`:

- `REG_STRIDE_REGS`: number of `vec4<u32>` registers per invocation
- `REG_STRIDE_BYTES = REG_STRIDE_REGS * 16`
- `linear_index = invocation_index * REG_STRIDE_REGS + reg_index`

In tessellation code, this addressing scheme is used for:

- `vs_out`: VS outputs per **input control point**
- `hs_out`: HS outputs per **output control point**
- `hs_patch_constants`: HS patch constants per **patch**

### HS tess factors (`hs_tess_factors`)

The tessellation layout pass consumes tess factors from a compact, per-patch buffer:

- element type: `vec4<f32>`
- count: `HS_TESS_FACTOR_VEC4S_PER_PATCH` `vec4<f32>` values per patch (currently 1)
- meaning for the current tri-domain integer path: `{edge0, edge1, edge2, inside}`

This buffer is written by the HS patch-constant phase (and today by the passthrough HS) and then
consumed by the deterministic serial layout pass (`runtime/tessellation/layout_pass.rs`).

### Expanded vertices + indices

The final render pass needs a WebGPU vertex buffer. Aero’s compute-expansion paths use an
“expanded vertex” format. There are currently **two** formats used in-tree:

1. **Storage-buffer ExpandedVertex record** (used by the current `aerogpu_cmd` expanded-draw path)
2. **Register-stride vertex buffer** (used by the in-progress tessellation DS evaluation templates)

#### 1) Storage-buffer ExpandedVertex record (current expanded-draw path)

This format matches the storage-buffer ABI expected by the autogenerated passthrough VS
(`wgsl_link::generate_passthrough_vs_wgsl`), and is what the current compute-prepass placeholder
shaders write:

```wgsl
struct ExpandedVertex {
  pos: vec4<f32>,                       // SV_Position (clip space)
  varyings: array<vec4<f32>, 32>,        // v0..v31 / @location(0..31)
}
```

Notes:

- `32` is `EXPANDED_VERTEX_MAX_VARYINGS` in `crates/aero-d3d11/src/binding_model.rs`.
- The stride is `(1 + EXPANDED_VERTEX_MAX_VARYINGS) * 16` bytes.
- The buffer is bound as `var<storage, read>` at
  `@group(BIND_GROUP_INTERNAL_EMULATION) @binding(BINDING_INTERNAL_EXPANDED_VERTICES)` and is
  indexed by `@builtin(vertex_index)` (so it is *not* limited by WebGPU vertex attribute count).
- Index buffers are typically `u32` triangle lists with `STORAGE | INDEX` usage.

#### 2) Register-stride vertex buffer (tessellation DS templates)

Some tessellation building blocks currently treat the “expanded vertices” buffer as a **register
file**: a flat `array<vec4<f32>>` where each vertex owns `OUT_REG_COUNT` contiguous registers.

This is the format written by the DS evaluation WGSL template in
`crates/aero-d3d11/src/runtime/tessellation/domain_eval.rs`:

- element type: `vec4<f32>` (one “register” = 16 bytes)
- stride: `OUT_REG_COUNT * 16` bytes per vertex

This format is convenient for stage linking (DS outputs are literally `o0..oN` registers), but it
requires a **vertex-buffer attribute** passthrough strategy (each register becomes a `@location`
vertex input) or a conversion pass to the storage-buffer `ExpandedVertex` record.

### Indirect draw args

Compute expansion avoids CPU readback by writing a single indirect args struct at offset 0. Aero
uses the canonical layouts in `crates/aero-d3d11/src/runtime/indirect_args.rs`:

- `DrawIndirectArgs` (16 bytes) for `draw_indirect`
- `DrawIndexedIndirectArgs` (20 bytes) for `draw_indexed_indirect`

### Per-patch offsets and `PatchMeta`

Because WebGPU cannot allocate buffers dynamically on the GPU, tessellation must write into
pre-allocated output buffers. The tessellator layout pass establishes per-patch offsets using a
deterministic prefix sum:

- `vertex_base`/`index_base` are element offsets (not bytes) into the expanded buffers.
- `vertex_count`/`index_count` encode the patch’s contribution.
- the layout pass also writes the final `DrawIndexedIndirectArgs` total counts.

See:

- Rust layout: `TessellationLayoutPatchMeta` in `crates/aero-d3d11/src/runtime/tessellation/mod.rs`
- WGSL layout pass: `crates/aero-d3d11/src/runtime/tessellation/layout_pass.rs`

---

## Bind group model

### Stage-scoped bind groups + `@group(3)` for extended stages

Aero’s DXBC→WGSL translation and command-stream executor share a stable binding model:

- `@group(0)`: VS resources
- `@group(1)`: PS resources
- `@group(2)`: CS resources

WebGPU guarantees `maxBindGroups >= 4`, so Aero uses `@group(3)` as a reserved internal/emulation
group (`BIND_GROUP_INTERNAL_EMULATION`) that hosts:

- **extended D3D stages**: GS/HS/DS resources (bound via `stage_ex`), and
- internal emulation helpers (vertex pulling, expansion scratch, indirect args, etc).

Within a group, binding numbers are derived from D3D register indices (see
`crates/aero-d3d11/src/binding_model.rs`), and internal bindings use
`@binding >= BINDING_BASE_INTERNAL` to avoid collisions.

### `stage_ex` and binding the HS/DS tables

To support additional D3D programmable stages (HS/DS) without breaking the ABI, some
resource-binding packets overload their trailing reserved field as a **`stage_ex` selector** when
`shader_stage == COMPUTE` (see `drivers/aerogpu/protocol/aerogpu_cmd.h`).

This extension was introduced in the command stream ABI **1.3** (minor = 3). When decoding command
streams with ABI minor < 3, hosts must ignore `reserved0` even when `shader_stage == COMPUTE`, to
avoid misinterpreting legacy reserved data.

Packets that currently support the `stage_ex` encoding:

- `CREATE_SHADER_DXBC`
- `SET_TEXTURE`
- `SET_SAMPLERS`
- `SET_CONSTANT_BUFFERS`
- `SET_SHADER_RESOURCE_BUFFERS` (SRV buffers, `t#` where the SRV is a buffer view)
- `SET_UNORDERED_ACCESS_BUFFERS` (UAV buffers, `u#` where the UAV is a buffer view)
- `SET_SHADER_CONSTANTS_F`

Encoding invariant:

- If `shader_stage != COMPUTE`, the `stage_ex`/`reserved0` field must be 0 and is ignored.
- If `shader_stage == COMPUTE`:
  - `stage_ex == 0` means the real/legacy Compute stage.
  - `stage_ex != 0` means `stage_ex` is present and encodes a non-zero DXBC program type selector.

- Preferred GS encoding: `shader_stage = GEOMETRY`, `stage_ex = 0`.
- `stage_ex` encoding (required for HS/DS; may also be used for GS for compatibility):
  - `2 = Geometry`
  - `3 = Hull`
  - `4 = Domain`

Pixel shaders are intentionally not representable via `stage_ex` because `0` is reserved for legacy
compute packets; pixel bindings always use `shader_stage = PIXEL`.

On the host, these are tracked as distinct per-stage binding tables so that “real compute” state is
not overwritten by graphics emulation state.

Implementation references:

- protocol enums/encoding: `emulator/protocol/aerogpu/aerogpu_cmd.rs`
- executor decoding: `ShaderStage::from_aerogpu_u32_with_stage_ex` in
  `crates/aero-d3d11/src/runtime/bindings.rs`

### Internal resources: per-pass internal groups (and the “group 3” exception)

Tessellation emulation needs many non-D3D bindings (register files, counters, patch metadata,
domain points, vertex pulling inputs). These are **not** part of the guest-visible D3D binding
model.

Decision: internal resources are bound using **per-pass internal bind groups** with layouts owned by
the executor, rather than reserving additional global bind-group indices.

In practice, there are two patterns in the current codebase:

1. **Separate internal group (preferred when possible)**  
   Example: DS evaluation uses `@group(0)` for internal buffers and `@group(3)` for DS resources
   (`DOMAIN_EVAL_INTERNAL_GROUP = 0`, `DOMAIN_EVAL_DOMAIN_GROUP = 3` in
   `runtime/tessellation/domain_eval.rs`).
2. **Share `@group(3)` for internal + stage resources**  
   Some kernels reuse `@group(3)` for everything because:
   - vertex pulling is already defined to live at `VERTEX_PULLING_GROUP = 3`, and/or
   - HS kernels currently build bind groups via the generic D3D binding-provider machinery and bind
     internal scratch buffers via `internal_buffer()` (see the comment in
     `runtime/tessellation/hull.rs`).

Both approaches preserve the key ABI invariant: D3D register-space bindings (`b#/t#/s#/u#`) remain
stable, and internal resources are always in executor-owned layouts/ranges.

---

## Limits, clamps, and tuning knobs

Tessellation can amplify geometry dramatically. The emulation must enforce deterministic limits to
avoid unbounded scratch allocations or invalid dispatch sizes.

### Tess factor clamps

- D3D11 hardware tessellation clamps to a maximum factor of **64** (`MAX_TESS_FACTOR` in
  `runtime/tessellator.rs`).
- The current WebGPU emulation path additionally clamps tessellation to a smaller, conservative
  limit: `MAX_TESS_FACTOR_SUPPORTED` (currently **16**) in `runtime/tessellation/mod.rs`.

Raising `MAX_TESS_FACTOR_SUPPORTED` increases scratch usage and can easily exceed default scratch
budgets; do so together with scratch sizing/tuning.

### Output-budget clamps (scratch guardrails)

The GPU layout pass (`runtime/tessellation/layout_pass.rs`) is parameterized by:

- `patch_count`
- `max_vertices`
- `max_indices`

It will clamp the derived tess level down per patch to keep the total expanded output within these
budgets, and writes a debug flag when clamping occurs.

On the CPU, `TessellationRuntime::alloc_draw_scratch` computes conservative worst-case sizes and
returns a structured `TessellationScratchOomError` when scratch capacity is insufficient.

### Scratch allocator tuning

`ExpansionScratchAllocator` is configured by `ExpansionScratchDescriptor`:

- `per_frame_size`: increase for tessellation-heavy workloads (default is small because most draws
  don’t yet use GS/HS/DS emulation)
- `frames_in_flight`: typically 2–3; should be ≥ the number of GPU frames that can be in flight to
  avoid reuse hazards

### Dispatch-size limits

HS/DS compute passes must respect `device.limits().max_compute_workgroups_per_dimension`.
Host-side helpers like `compute_dispatch_x` in `runtime/tessellation/hull.rs` validate this and
error early with actionable messages.

---

## Testing strategy (and adding fixtures)

Tessellation emulation has two orthogonal correctness surfaces:

1. **Tessellator math/layout correctness** (domain points + connectivity + clamping).
2. **End-to-end pipeline plumbing** (bindings, scratch offsets, indirect args, stage linking).

### Unit tests (CPU-side, deterministic)

- `crates/aero-d3d11/src/runtime/tessellator.rs`: tri-domain integer tessellation math tests.
- `crates/aero-d3d11/src/runtime/tessellation/buffers.rs`: sizing/overflow tests.

### GPU tests (building blocks)

- `crates/aero-d3d11/tests/tessellation_layout_pass.rs`: runs WGSL layout pass and validates patch
  meta + indirect args.
- `crates/aero-d3d11/tests/tessellation_tri_domain_integer_index_gen.rs`: validates GPU index
  generation against the CPU reference tessellator.
- `crates/aero-d3d11/tests/tessellation_vs_as_compute.rs`: VS-as-compute vertex pulling + register
  addressing.
- `crates/aero-d3d11/tests/tessellation_scratch_guardrails.rs`: validates scratch OOM errors include
  computed sizes and clamped tess factors.

### Executor tests (command-stream integration)

- `crates/aero-d3d11/tests/aerogpu_cmd_tessellation_smoke.rs`: patchlist+HS/DS routes through the
  tessellation compute prepass.
- `crates/aero-d3d11/tests/aerogpu_cmd_tessellation_hs_ds_compute_prepass_error.rs`: despite the
  name, this currently documents early tessellation prepass error policy (e.g. missing input
  layouts) without panicking.
- `crates/aero-d3d11/tests/aerogpu_cmd_stage_ex_bindings_hs_ds.rs`: validates `stage_ex` routing for
  HS/DS resource binding packets.

### Adding HS/DS DXBC fixtures

Fixture binaries live in `crates/aero-d3d11/tests/fixtures/` (see `README.md` in that directory).
The repo already includes:

- `hs_minimal.dxbc`, `hs_tri_integer.dxbc`
- `ds_tri_passthrough.dxbc`, `ds_tri_integer.dxbc`

To add new tessellation fixtures:

1. Keep them tiny and deterministic (avoid texture sampling for early tests).
2. Prefer constant tess factors and simple interpolation patterns.
3. Add the new `hs_*.dxbc` / `ds_*.dxbc` files to the fixtures directory.
4. Document the behavior and (if applicable) the compilation command in
   `crates/aero-d3d11/tests/fixtures/README.md`.

---

## Future work (expected extensions)

- **Quad domain** tessellation (different domain coordinate mapping + index generation).
- **Isoline domain** tessellation.
- **Fractional partitioning** (`fractional_even`, `fractional_odd`, `pow2`) with D3D-compatible
  rounding rules.
- **Performance work**: fuse passes, cache index patterns, use workgroup shared memory for control
  points/patch constants, reduce register-file bandwidth by packing only live outputs.
