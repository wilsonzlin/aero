# Geometry Shader Emulation (D3D10/11 GS → WebGPU)

WebGPU does **not** expose a geometry shader (GS) stage. Aero’s strategy is to **emulate GS via compute**
by expanding primitives into intermediate buffers, then drawing those buffers with a normal WebGPU render
pipeline.

This document describes:

- what is **implemented today** (command-stream plumbing, binding model, compute-expansion/compute-prepass scaffolding + current limitations; plus a minimal SM4 GS DXBC→WGSL compute path that is executed for a small set of IA input topologies (`PointList` and `TriangleList`) via the translated GS prepass), and
- the **next steps** (broaden VS-as-compute feeding for GS inputs (currently minimal; opcode coverage + instancing), then grow opcode/system-value/resource-binding coverage and bring up HS/DS emulation).

> Related: [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md) (high-level D3D10/11→WebGPU mapping).
>
> Related: [`docs/graphics/tessellation-emulation.md`](./tessellation-emulation.md) (HS/DS emulation pipeline).

---

## Why emulation is required

Direct3D 10/11 pipelines can contain:

```
IA -> VS -> GS -> Rasterizer -> PS -> OM
```

WebGPU render pipelines only have:

```
Vertex -> Fragment -> OM
```

There is no GS equivalent, and WebGPU does not provide transform-feedback/stream-out to capture vertex
shader outputs as a buffer for later stages. As a result, when a GS is active Aero must:

1. **Run the VS as compute** (so we can explicitly write outputs to a storage buffer).
2. **Run the GS as compute** (reading VS outputs and writing expanded primitives).
3. **Render** the expanded primitives using a small passthrough vertex shader + the original pixel shader.

---

## Emulation pipeline (compute expansion)

### High-level flow

When a geometry shader is active, a draw is executed as:

1. **Vertex pulling + VS-as-compute**
   - Read IA vertex buffers and index buffers manually (“vertex pulling”).
   - Execute the D3D VS logic in a compute entry point.
   - Write `VsOut` structs into an intermediate storage buffer (one per input vertex invocation).

2. **GS-as-compute**
   - Assemble input primitives (point/line/triangle) from the post-VS data.
   - Execute the D3D GS logic as compute, using `EmitVertex` / `CutVertex` semantics.
   - Write expanded vertices into an output vertex buffer (storage).
   - Some prepasses also write an expanded index buffer (indexed list form), which the executor may
     expand into a dense vertex stream before rendering; the current synthetic-expansion fallback
     prepass only emits non-indexed vertices and uses `draw_indirect`.
   - Write an **indirect draw args** struct so the subsequent render pass can draw without CPU readback.

3. **Render expanded geometry**
   - Bind a render pipeline consisting of:
     - a small **passthrough VS** that reads the expanded vertex buffer, and
     - the original D3D pixel shader (translated to WGSL fragment).
   - Issue `draw_indirect` using the args buffer produced by step (2).
     - If the compute prepass produced an expanded index buffer, the executor first expands the
       indexed output into a dense (non-indexed) vertex stream so the render pass can stay
       non-indexed. This avoids relying on `draw_indexed_indirect` on downlevel backends.

Current status:

- The executor routes draws through a compute prepass when GS/HS/DS stages are bound (or when D3D11-only
  topologies like adjacency/patchlists are used).
- Patchlist draws without HS/DS and many GS cases still use built-in WGSL (“synthetic expansion”) to
   generate expanded geometry.
- Patchlist draws with HS+DS bound route through the tessellation prepass pipeline (VS-as-compute +
  HS/DS passthrough + tessellator layout + DS passthrough).
- There is an initial “real GS” path for **a small set of input-assembler (IA) topologies** (`PointList` and `TriangleList`) for both `Draw` and `DrawIndexed`:
  if the bound GS DXBC can be translated by `crates/aero-d3d11/src/runtime/gs_translate.rs`, the executor
  executes that translated WGSL compute prepass at draw time.
  - Today, GS `v#[]` inputs are populated via vertex pulling:
    - Translated-GS prepass paths prefer feeding `v#[]` from **VS outputs**
      via vertex pulling plus a minimal **VS-as-compute** implementation (simple SM4 subset). If
      VS-as-compute translation fails, draws fail unless the VS is a strict passthrough (or
      `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is set to force IA-fill for debugging; may misrender).
  - This requires an input layout.

### Why we expand strips into lists

D3D GS outputs are typically declared as `line_strip` or `triangle_strip`, and can use `CutVertex`
to terminate the current strip and start a new one.

For simplicity and portability, Aero’s emulation expands strips into lists:

- `line_strip` → **line list** (each new vertex after the first emits one line segment)
- `triangle_strip` → **triangle list** (each new vertex after the first two emits one triangle)

This avoids needing to generate restart indices and keeps the draw stage in the most widely-supported
primitive topologies.

Note: there is an in-tree GS→WGSL compute translator at `crates/aero-d3d11/src/runtime/gs_translate.rs`.
It supports `pointlist`, `linestrip`, and `trianglestrip` GS output topologies. Strip topologies are
lowered to indexed list topologies (`linestrip` → **line list**, `trianglestrip` → **triangle list**)
suitable for indexed list drawing.
The executor currently converts indexed prepass outputs into a non-indexed vertex stream before
rendering so it can always use `draw_indirect` (avoiding `draw_indexed_indirect` on downlevel
backends).
It is partially wired into the command executor via the translated-GS prepass paths for `PointList`
and `TriangleList`; other cases still fall back to synthetic expansion.

---

## Current implementation status (AeroGPU command-stream executor)

The AeroGPU D3D10/11 command-stream executor implements GS emulation as a GPU-side **compute expansion
prepass** + **indirect draw** path. It also has an initial “execute guest GS DXBC” path for a small
subset of IA input topologies (`PointList` and `TriangleList`), but it is not yet a complete GS implementation.

There are currently three compute-prepass “modes”:

- **Tessellation emulation (bring-up):** for patchlist draws with HS+DS bound, run a multi-pass
  tessellation prepass (VS-as-compute vertex pulling + HS passthrough + tessellator layout + DS
  passthrough) to expand patches into an indexed triangle list.
- **Real GS execution (supported subset):** translate the guest’s GS DXBC into a WGSL compute shader
  and run it to generate expanded vertices/indices + indirect args.
- **Fallback synthetic expansion (scaffolding):** run `GEOMETRY_PREPASS_CS_WGSL`, which emits synthetic primitives. This
  mode remains useful for patchlist scaffolding and for tests that force the compute-prepass
  path without a real GS.

### Synthetic expansion vs translated GS prepass (current behavior)

The executor currently uses **two distinct** compute prepass implementations for GS-like expansion:

#### Built-in synthetic-expansion prepass (`GEOMETRY_PREPASS_CS_WGSL`)

- **What it does:** emits deterministic synthetic primitives (triangles) and writes indirect draw args.
- **What it does *not* do:** it does *not* execute any guest GS DXBC.
- **Dispatch shape:** `dispatch_workgroups(primitive_count, gs_instance_count, 1)`.
  - The WGSL treats:
    - `global_invocation_id.x` as a synthetic `SV_PrimitiveID`, and
    - `global_invocation_id.y` as a synthetic `SV_GSInstanceID` (used by GS instancing tests).
- **Bindings:**
  - `@group(0)` contains prepass IO:
    - expanded vertices (`out_vertices`),
    - packed indirect+counter state (`out_state`, sized by `GEOMETRY_PREPASS_PACKED_STATE_SIZE_BYTES`), and
    - small uniform parameters.
  - `@group(3)` provides the GS stage resource table (`cb#/t#/s#`) and (optionally) internal IA vertex pulling
    bindings.
- **Used for:**
  - patchlist scaffolding,
  - tessellation bring-up before real HS/DS execution exists, and
  - tests that validate compute-prepass+indirect plumbing without requiring a translated GS (many such
    tests force the emulation path by using an adjacency topology that WebGPU cannot draw directly).

#### Translated GS DXBC prepass (`runtime/gs_translate.rs`)

- **What it does:** executes a supported subset of guest GS DXBC as WGSL compute to produce expanded
  vertices/indices and indirect args.
- **When it runs:** for draws using a supported IA input topology (`PointList` or `TriangleList`) where the bound
  GS DXBC successfully translated at `CREATE_SHADER_DXBC` time.
 - **Pass sequence (translated-GS prepass paths today):**
   1. **Input fill:** a compute pass populates the packed `gs_inputs` payload from **VS outputs**, using
      vertex pulling to load IA data and a small VS-as-compute translator (currently a minimal SM4 subset)
      to execute the guest VS instruction stream for the subset needed by GS tests.
      - If VS-as-compute translation fails, the executor only falls back to filling `gs_inputs` from the
        IA stream when the VS is a strict passthrough (or `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is set
        to force IA-fill for debugging; may misrender). Otherwise the draw fails with a clear error.
   2. **GS execution:** the translated GS WGSL compute entry point runs once per input primitive
      (`dispatch_workgroups(primitive_count, 1, 1)`) and loops `gs_instance_id` in `0..GS_INSTANCE_COUNT`.
      It appends outputs using atomics, performing strip→list conversion and honoring `cut` semantics.
   3. **Finalize:** a 1-workgroup dispatch runs `cs_finalize` to write `DrawIndexedIndirectArgs` from the
      counters (and to deterministically skip the draw if overflow occurred).
- **Bindings (translated GS prepass WGSL):**
  - `@group(0)` contains prepass IO (expanded vertices/indices, counters+indirect args, params, and
    `gs_inputs`).
  - `@group(3)` contains referenced GS stage resources (`cb#`, `t#`, `s#`) following the shared binding
    model in `binding_model.rs`.
  - The IA vertex pulling bindings (`@group(3) @binding >= BINDING_BASE_INTERNAL`) are only required by
    the **input fill** pass, not by the translated GS itself.

Implemented today:

- **GS/HS/DS bindings**: resource-binding opcodes can target GS (and future HS/DS) binding tables
  without clobbering compute-stage bindings (see “Resource binding model” below). GS can be
  addressed either via `shader_stage = GEOMETRY` (preferred) or via the `stage_ex` compatibility
  encoding; HS/DS require `stage_ex`.
- **Extended `BIND_SHADERS`**: the `BIND_SHADERS` packet can carry `gs/hs/ds` handles via an
  append-only tail (when present, the appended handles are authoritative), and draws route through a
  dedicated “compute prepass” path when any of these stages are bound.
- **Compute→indirect→render pipeline plumbing**: the executor runs a compute prepass to write an
   expanded buffer(s) + indirect args, then renders via `draw_indirect`
   (see `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
  - **Translated GS prepass (real GS subset):** for `PointList` and `TriangleList` draws, a supported subset of SM4 GS DXBC is translated to WGSL compute and executed to
    produce expanded geometry (see `exec_geometry_shader_prepass_*` in
    `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
    This prepass writes:
    - expanded vertices,
    - expanded indices, and
    - indirect args (packed with counters into a single storage buffer binding to stay within the
      downlevel WebGPU `max_storage_buffers_per_shader_stage = 4` budget).

    The executor then expands the indexed output into a non-indexed vertex stream and the subsequent
    render pass draws via `draw_indirect` (avoiding `draw_indexed_indirect` on downlevel backends).
  - **Synthetic-expansion prepass (fallback/scaffolding):** the built-in WGSL prepass is used as a
    fallback and for bring-up coverage tests (see `GEOMETRY_PREPASS_CS_WGSL` /
    `GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL` in
    `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).

    This prepass writes:
    - expanded vertices, and
    - a packed `out_state` storage buffer containing indirect args + counters (sized by
      `GEOMETRY_PREPASS_PACKED_STATE_SIZE_BYTES`).

    It intentionally does **not** write expanded indices (to reduce storage buffer bindings under
    downlevel limits), so the executor always renders this path via `draw_indirect` (even for
    `DRAW_INDEXED` commands).
- **GS DXBC → WGSL compute translation (minimal subset)**:
  - GS DXBC is decoded to SM4 IR and translated to WGSL compute in
    `crates/aero-d3d11/src/runtime/gs_translate.rs` (invoked from `CREATE_SHADER_DXBC` for GS).
  - Strip-cut (`CutVertex` / `RestartStrip`) semantics are validated by deterministic reference
    implementations in `crates/aero-d3d11/src/runtime/strip_to_list.rs`.

Current limitations (high-level):

- Only a small “real GS” path is implemented today:
  - `PointList` and `TriangleList` draws can execute translated SM4 GS DXBC as the compute prepass when
    the shader is within the supported translator subset.
  - Other cases that route through compute-based emulation (including adjacency/patchlist topologies)
    still use the built-in synthetic expansion WGSL prepass (and do not execute guest GS DXBC).
- VS-as-compute feeding for GS inputs is still incomplete:
  - The translated-GS prepass paths prefer a minimal VS-as-compute feeding path so the GS observes VS
    output registers (correct D3D11 semantics), but it is still a small subset (simple VS expected).
  - If VS-as-compute translation fails, the executor only falls back to IA-fill when the VS is a
    strict passthrough (or `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is set to force IA-fill for
    debugging; may misrender). Otherwise the draw fails with a clear error.
- HS/DS are still scaffolding-only (no real HS/DS DXBC execution yet).

Test pointers:

- End-to-end translated GS execution:
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_pointlist_draw_indexed.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_feeds_gs_inputs.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_emits_triangle.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_reads_srv_buffer_translated_prepass.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_texture_t0_translated_prepass.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_samples_texture_translated_prepass.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_translated_primitive_id.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_group3_resources.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_translate_cbuffer_cb1.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_cbuffer_b0_offsets_prepass.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_vs_as_compute_feeds_gs_inputs.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_output_topology_pointlist.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_cbuffer_b0_translated_prepass.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_line_strip_output.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_gs_emulation_passthrough.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_gs_instance_count.rs`
- Compute prepass plumbing (synthetic expansion): `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs`
  (and `*_primitive_id.rs`, `*_vertex_pulling.rs`, etc)
- GS translator unit tests (standalone): `crates/aero-d3d11/tests/gs_translate.rs`
- DXBC tooling (opcode discovery / token shapes): `cargo run -p aero-d3d11 --bin dxbc_dump -- <gs_*.dxbc>`

---

## Supported GS feature subset (initial)

This section documents the *actual* supported subset for end-to-end GS execution (guest DXBC →
WGSL compute → expanded draw). Anything not listed here should be assumed unsupported.

### Input primitive types (end-to-end)

Supported end-to-end today (translated-GS prepass):

- `point`: `D3D11_PRIMITIVE_TOPOLOGY_POINTLIST`
- `triangle`: `D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST`

Not yet supported end-to-end (these may still route through synthetic expansion for plumbing tests, but do not execute guest GS DXBC):

- `line`: `D3D11_PRIMITIVE_TOPOLOGY_LINELIST`, `D3D11_PRIMITIVE_TOPOLOGY_LINESTRIP`
- `triangle`: `D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP`
- adjacency (`*_ADJ` / `lineadj` / `triadj`)

Note: for the current translated-GS prepass paths, the GS `v#[]` inputs are populated via vertex
pulling plus a minimal VS-as-compute feeding path (simple SM4 subset) so the GS observes VS output
registers (correct D3D11 semantics). If VS-as-compute translation fails, the executor only falls
back to IA-fill when the VS is a strict passthrough (or `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is
set to force IA-fill for debugging; may misrender). Otherwise the draw fails with a clear error.

Note: adjacency topologies require adjacency-aware IA primitive assembly. When adjacency is implemented
end-to-end, the required vertex ordering for `LINELIST_ADJ`/`LINESTRIP_ADJ` and
`TRIANGLELIST_ADJ`/`TRIANGLESTRIP_ADJ` is specified in [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md) section 2.1.1b.

### Output topology / streams

Supported end-to-end today (for the translated-GS prepass paths):

- `pointlist` output (indexed **point list**, rendered as `PointList`)
- `linestrip` output, lowered to an indexed **line list** (rendered as `LineList`)
- `trianglestrip` output, lowered to an indexed **triangle list** (rendered as `TriangleList`)
- only **stream 0**

Not yet supported:

- multi-stream output (`EmitStream` / `CutStream` / `SV_StreamID`)

### Supported instruction subset

Supported instructions/opcodes:

- **Primitive emission**
  - `EmitVertex` (`emit`)
  - `CutVertex` (`cut`)
  - `EmitVertex` + `CutVertex` (`emitthen_cut`)
- **Predication / predicate registers (subset)**
  - `setp` (predicate write; `p#` registers)
  - DXBC instruction predication for non-control-flow instructions (emitted as WGSL `if` wrappers)
- **Structured control flow**
  - `if` (`if_z` / `if_nz`)
  - `ifc` (`ifc` compare variants, including unsigned comparisons)
  - `else` / `endif`
  - `loop` / `endloop`
  - `break` / `continue`
  - `breakc` / `continuec`
  - `switch` / `case` / `default` / `endswitch`
  - `ret`
- **ALU (subset)**
  - `mov`, `movc`
  - `add`, `mul`, `mad`
  - `dp3`, `dp4`
  - `min`, `max`
  - `rcp`, `rsq`
  - integer/bitfield ops (subset): `iadd`, `isub`, `ishl`, `ishr`, `ushr`, `imin`, `imax`, `umin`,
    `umax`, `iabs`, `ineg`, `cmp`, `udiv`, `idiv`, `bfi`, `ubfe`, `ibfe`, `bfrev`, `countbits`,
    `firstbit_hi`, `firstbit_lo`, `firstbit_shi`
  - `and`/`or`/`xor`/`not` (bitwise ops on raw 32-bit lanes)
  - conversions: `itof`, `utof`, `ftoi`, `ftou`, `f32tof16`, `f16tof32`
- **Resource reads (subset)**
  - 2D textures:
    - `sample`, `sample_l` (`Texture2D.Sample*`)
    - `ld` (`Texture2D.Load`)
    - `resinfo` (`Texture2D.GetDimensions` / `Texture2D.GetDimensions` + mip count)
  - SRV buffers:
    - `ld_raw`
    - `ld_structured`
    - `bufinfo` (`ByteAddressBuffer.GetDimensions` / `StructuredBuffer.GetDimensions`)

Supported operand surface (initial):

- temp regs (`r#`) and output regs (`o#`) (note: `o0` is treated as `SV_Position` and stored in
  `ExpandedVertex.pos`; non-position outputs `oN` are exported to `ExpandedVertex.varyings[N]` for the
  set of output registers declared/written by the GS; unwritten slots default to zero)
- GS inputs via `v#[]` (no vertex index out of range for the declared input primitive)
- constant buffers (`cb#[]`) for statically indexed reads (requires `dcl_constantbuffer`)
- resources used by the supported read-only ops above:
  - `t#` Texture2D (requires `dcl_resource_texture2d`)
  - `t#` SRV buffer (requires `dcl_resource_buffer`)
  - `s#` sampler (requires `dcl_sampler`)
- immediate32 `vec4` constants (treated as raw 32-bit lane values; typically `f32` bit patterns)
- swizzles, write masks, destination saturate (`_sat`), and basic operand modifiers (`abs` / `-` / `-abs`)
- system values:
  - `SV_PrimitiveID`
  - `SV_GSInstanceID` (honors `dcl_gsinstancecount` / `[instance(n)]`; the translated prepass loops
    `0..GS_INSTANCE_COUNT` per input primitive, values `0..(n-1)`; default is `n=1`, so the ID is
    always `0`)

Unsupported today (non-exhaustive): resource writes/stores/UAVs, barrier/synchronization opcodes
(`sync`), and most other SM4/SM5 instructions. Unsupported features fail translation with a clear
error.

---

## Current limitations / non-goals

Geometry shader emulation is intentionally *not* a full D3D11 GS implementation in its first version.
Known limitations include:

- **No multi-stream output**
  - No `EmitStream` / `CutStream` / `emit_stream` / `cut_stream`
  - Only stream 0 is supported; non-zero stream indices are rejected (fail-fast) at
    `CREATE_SHADER_DXBC` time.
- **No stream-out (SO / transform feedback)**
  - GS output cannot be captured into D3D stream-out buffers
- **Limited VS-as-compute feeding for GS inputs**
  - The translated-GS prepass paths run a small VS-as-compute translator to populate the GS `v#[]`
    register payload from VS outputs. This currently supports a small VS instruction subset (enough
    for the in-tree GS tests) and will fail translation for more complex vertex shaders.
  - If VS-as-compute translation fails, the executor only falls back to IA-fill when the VS is a strict
    passthrough (or `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is set to force IA-fill for debugging; may
    misrender). Otherwise the draw fails with a clear error.
- **Draw instancing (`instance_count > 1`) is not validated**
  - The emulation path preserves `instance_count` in the indirect draw args, but the current translated
    GS prepass does not expand geometry per draw instance and does not currently fan out over
    `SV_InstanceID`. Treat instanced draws with GS bound as unsupported until dedicated tests exist.
- **Limited output topology / payload**
  - Output topology is limited to `pointlist`, `linestrip`, and `trianglestrip` (stream 0 only).
    Strip topologies are lowered to list topologies for rendering (`linestrip` → line list,
    `trianglestrip` → triangle list).
  - The expanded-vertex record stores `SV_Position` plus up to 32 `@location(N)` varyings
    (`vec4<f32>` each, indexed by location). The translated GS prepass exports non-position output
    registers `oN` to `ExpandedVertex.varyings[N]` for the set of output registers declared/written by
    the GS; varyings that are not written default to zero.
- **No layered rendering semantics**
  - No `SV_RenderTargetArrayIndex` / `SV_ViewportArrayIndex` style outputs (future work)
- **No fixed-function GS-side rasterizer discard**
  - WebGPU does not expose rasterizer discard; the emulation always runs the render pass
- **WebGL2 backend**
  - WebGL2 has no compute; GS emulation is WebGPU-only (or requires a separate CPU fallback path)
- **Downlevel per-stage storage-buffer limits**
  - Some downlevel backends have very low `max_storage_buffers_per_shader_stage` (commonly 4). GS
    emulation binds multiple storage buffers (expanded vertices + optional expanded indices,
    counters/indirect args, and sometimes IA pulling buffers), so some prepass variants may not be
    available on those backends.
    See `crates/aero-d3d11/tests/common/mod.rs` (`skip_if_compute_or_indirect_unsupported`) for the
    current skip heuristics used by tests.

Error policy:

- Some unsupported GS features are rejected with clear errors (e.g. non-zero stream indices at
  `CREATE_SHADER_DXBC` time).
- If the guest GS DXBC cannot be translated by the current `gs_translate` subset, the GS handle is
  still accepted/stored, but draws with that GS bound currently fail with a clear
  “geometry shader not supported” error (rather than silently running the synthetic-expansion
  prepass).
- The synthetic-expansion prepass is intended for scaffolding/tests and for non-GS cases that still
  need the compute-prepass path; it is not meant as a “compatibility fallback” for arbitrary
  unsupported GS bytecode.

---

## Synthetic-expansion prepass (`GEOMETRY_PREPASS_CS_WGSL`)

Even with real GS execution available for a small subset, the executor keeps a **synthetic-expansion**
compute prepass (`GEOMETRY_PREPASS_CS_WGSL` in
`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`) for cases where the command stream must
route through the emulation path but there is no real GS/HS/DS kernel to run yet.

### Output layout (packed state) and indirect draw behavior

The synthetic-expansion fallback prepasses (`GEOMETRY_PREPASS_CS_WGSL` and
`GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL`) write:

- `out_vertices`: the expanded vertex buffer consumed by the emulation passthrough VS.
- `out_state`: a single packed storage buffer containing:
  - indirect args, and
  - a small counter block reserved for future GS/HS/DS emulation bookkeeping.

The packed `out_state` buffer is sized by `GEOMETRY_PREPASS_PACKED_STATE_SIZE_BYTES` (see
`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`). Packing args + counters into a single
binding (and omitting an expanded index buffer in this fallback path) is motivated by downlevel
WebGPU’s tight storage-buffer binding budget (`max_storage_buffers_per_shader_stage = 4` in
`wgpu::Limits::downlevel_defaults()`).

Because this fallback prepass does not produce an expanded index buffer, the executor always draws it
via `draw_indirect` (including when the original command was `DRAW_INDEXED`).

Current uses:

- **HS/DS scaffolding:** bring-up work for tessellation uses the same “compute prepass + indirect draw”
  shape, even before HS/DS DXBC execution exists.
- **Patchlist scaffolding:** D3D11 patchlist topologies (`*_PATCHLIST_*`) are routed through the
  emulation path even before full tessellation is available.
- **Tests that force emulation:** adjacency topologies (`*_ADJ`) are commonly used by tests to force the
  compute-prepass path (because WebGPU cannot draw adjacency primitives directly). Adjacency is not yet
  supported by the translated-GS prepass, so these draws currently route through the synthetic-expansion
  fallback.
- **Tests that force compute-prepass without a real GS:** e.g.
  `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs` binds a dummy GS
  handle and patches the topology to a patchlist value to validate the executor path.

---

## How to test

GS-related tests use checked-in DXBC fixtures under `crates/aero-d3d11/tests/fixtures/`.
See [`crates/aero-d3d11/tests/fixtures/README.md`](../../crates/aero-d3d11/tests/fixtures/README.md)
for details (including how the GS fixtures are authored and how to dump token streams with
`dxbc_dump`).

End-to-end GS emulation (compute prepass executes guest GS DXBC) is covered by:

- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_emits_triangle.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_feeds_gs_inputs.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_reads_srv_buffer_translated_prepass.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_texture_t0_translated_prepass.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_samples_texture_translated_prepass.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_translated_primitive_id.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_vs_as_compute_feeds_gs_inputs.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_pointlist_draw_indexed.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_gs_instance_count.rs`

These tests require compute shaders and indirect execution, so they may skip on downlevel backends
(e.g. WebGL2, or wgpu-GL adapters with low storage-buffer binding limits).

Example:

```bash
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_point_to_triangle
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_restart_strip
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_trianglelist_emits_triangle
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_trianglelist_vs_as_compute_feeds_gs_inputs
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_reads_srv_buffer_translated_prepass
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_texture_t0_translated_prepass
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_samples_texture_translated_prepass
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_translated_primitive_id
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_vs_as_compute_feeds_gs_inputs
```

To make skips fail-fast in CI-like environments, set `AERO_REQUIRE_WEBGPU=1` (tests will panic rather
than printing “skipping ...”).

For synthetic-expansion/scaffolding coverage, see:

- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs`

---

## Resource binding model

### Bind group indices

Aero’s binding model is stage-scoped. In the AeroGPU command-stream executor (`crates/aero-d3d11/src/runtime/bindings.rs`):

- `@group(0)`: VS resources
- `@group(1)`: PS resources
- `@group(2)`: CS resources
- `@group(3)`: reserved internal / emulation group (keeps the total bind-group count within WebGPU’s
  baseline `maxBindGroups >= 4` guarantee):
  - GS/HS/DS resources (tracked separately from CS to avoid clobbering)
  - internal expansion helpers (vertex pulling, etc) using `@binding >= BINDING_BASE_INTERNAL` to
    avoid collisions with D3D `b#`/`t#`/`s#`/`u#` bindings.

GS/HS/DS stages are emulated using compute passes, but their **D3D-stage resource bindings** are
tracked independently and are expected to be provided to the emulation pipelines via a reserved
bind group:

- `@group(3)` for GS/HS/DS resources (selected either via the direct `shader_stage = GEOMETRY`
  encoding for GS, or via the `stage_ex` ABI extension when `shader_stage = COMPUTE` (required for
  HS/DS; optional GS compatibility encoding)).
- Internal emulation helpers use a mix of:
  - **per-pass internal bindings** (most compute-prepass IO lives in `@group(0)` with small binding
    numbers), and
  - **reserved internal bindings in `@group(3)`** (for shared helpers like IA vertex pulling and the
    expanded-draw vertex buffer).
  Bindings in the internal reserved range start at `BINDING_BASE_INTERNAL = 256` (defined in
  `crates/aero-d3d11/src/binding_model.rs`) so they do not collide with D3D register bindings.

### Binding number ranges within a stage group

Within each stage’s bind group, D3D register spaces are mapped to disjoint `@binding` ranges:

| D3D register space | WGSL `@binding` | Notes |
|---|---:|---|
| `b#` / `cb#` | `BINDING_BASE_CBUFFER + slot` | constant buffers |
| `t#` | `BINDING_BASE_TEXTURE + slot` | SRV textures/buffers |
| `s#` | `BINDING_BASE_SAMPLER + slot` | samplers |
| `u#` | `BINDING_BASE_UAV + slot` | UAV buffers (SM5) |

Constants (current defaults):

- `BINDING_BASE_CBUFFER = 0`
- `BINDING_BASE_TEXTURE = 32`
- `BINDING_BASE_SAMPLER = 160`
- `BINDING_BASE_UAV = 176` (`160 + 16`)
- `MAX_UAV_SLOTS = 8` (`u0..u7`)

### Vertex pulling + expansion internal bindings (`@group(3)`)

When running VS/GS/HS/DS as compute, vertex attributes must be loaded from IA vertex buffers manually
(“vertex pulling”), and intermediate outputs must be written to scratch buffers.

Vertex pulling uses a dedicated bind group (`VERTEX_PULLING_GROUP` in
`crates/aero-d3d11/src/runtime/vertex_pulling.rs`):

- `@group(3) @binding(BINDING_BASE_INTERNAL)`: a small uniform containing per-slot `base_offset` + `stride` (+ draw params)
- `@group(3) @binding(BINDING_BASE_INTERNAL + 1 + i)`: vertex buffer slot `i` as a storage buffer (read-only)

These bindings are **internal** to the emulation path; they are not part of the D3D register binding model.
The broader compute-expansion pipeline also defines additional internal scratch bindings; see
[`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md) (including the reserved internal
binding-number range).

Note: the full GS/HS/DS emulation pipeline will need a unified bind-group layout that accommodates
 both GS/HS/DS D3D bindings (low `@binding` ranges) and vertex pulling/expansion internal bindings
 (`@binding >= BINDING_BASE_INTERNAL`) within `@group(3)` (keeping the bind group count within the
 WebGPU baseline of 4).

### AeroGPU command stream note: `stage_ex`

The AeroGPU command stream has legacy `shader_stage` enums that mirror WebGPU (VS/PS/CS) and also
includes an explicit Geometry stage (`shader_stage = GEOMETRY`).

To support additional D3D programmable stages (HS/DS) without breaking ABI, some packets support a
“stage_ex” extension that overloads the `reserved0` field when `shader_stage == COMPUTE` (see
`emulator/protocol/aerogpu/aerogpu_cmd.rs`).

This extension was introduced in the command stream ABI **1.3** (minor = 3). When decoding command
streams with ABI minor < 3, hosts must ignore `reserved0` even when `shader_stage == COMPUTE`, to
avoid misinterpreting legacy reserved data.

- Preferred GS encoding:
  - set `shader_stage = GEOMETRY` and `reserved0 = 0`
  - this avoids accidentally clobbering CS bindings on hosts that do not implement `stage_ex`
- `stage_ex` encoding (required for HS/DS; may also be used for GS for compatibility):
  - set `shader_stage = COMPUTE` (legacy value `2`)
  - set `reserved0` to a non-zero DXBC program type:
    - `1 = VS`, `2 = GS`, `3 = HS`, `4 = DS`, `5 = CS`
  - `reserved0 == 0` retains legacy compute semantics.
  - Pixel shaders use `shader_stage = PIXEL`; `stage_ex` cannot represent Pixel because `0` is
    reserved for legacy compute.

Packets that carry a `stage_ex` selector in `reserved0` include: `SET_TEXTURE`, `SET_SAMPLERS`,
`SET_CONSTANT_BUFFERS`, `SET_SHADER_RESOURCE_BUFFERS`, `SET_UNORDERED_ACCESS_BUFFERS`, and
`SET_SHADER_CONSTANTS_F`.

---

## Performance characteristics

GS emulation is significantly more expensive than native GS hardware support because it introduces:

- **Extra passes**: one or more compute passes (GS-input fill (IA-fill or VS-as-compute), GS itself, and
  potentially additional expansion passes as tessellation/adjacency coverage grows)
  before the render pass.
- **Intermediate buffers**: VS output + expanded vertex buffer (+ optional expanded index buffer) +
  indirect args/state.
- **Strip→list expansion cost**:
  - `triangle_strip` with `N` emitted vertices produces `(N-2)` triangles, i.e. **`3*(N-2)` list vertices**.
  - `line_strip` with `N` emitted vertices produces `(N-1)` segments, i.e. **`2*(N-1)` list vertices**.

In practice:

- GS-heavy workloads will be bandwidth-bound and should be expected to perform worse than on native D3D.
- The emulation path is best treated as a **compatibility** feature; “fast paths” (pattern-based lowering)
  may still be desirable later for common GS usage patterns.
