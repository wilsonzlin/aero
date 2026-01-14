# Geometry Shader Emulation (D3D10/11 GS → WebGPU)

WebGPU does **not** expose a geometry shader (GS) stage. Aero’s strategy is to **emulate GS via compute**
by expanding primitives into intermediate vertex/index buffers, then drawing those buffers with a normal
WebGPU render pipeline.

This document describes:

- what is **implemented today** (command-stream plumbing, binding model, compute-expansion/compute-prepass scaffolding + current limitations; plus a minimal SM4 GS DXBC→WGSL compute path that is executed for point-list and triangle-list draws (`Draw` and `DrawIndexed`)), and
- the **next steps** (expand GS DXBC execution beyond the current point-list and triangle-list subset, expand VS-as-compute feeding for GS inputs (currently minimal), then grow opcode/topology/system-value coverage and bring up HS/DS emulation).

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
   - Write an **indirect draw args** struct so the subsequent render pass can draw without CPU readback.

3. **Render expanded geometry**
   - Bind a render pipeline consisting of:
     - a small **passthrough VS** that reads the expanded vertex buffer, and
     - the original D3D pixel shader (translated to WGSL fragment).
   - Issue `draw_indirect` / `draw_indexed_indirect` using the args buffer produced by step (2).

Current status:

- The executor routes draws through a compute prepass when GS/HS/DS stages are bound (or when D3D11-only
  topologies like adjacency/patchlists are used).
- Patchlist draws without HS/DS and many GS cases still use built-in WGSL (“synthetic expansion”) to
  generate expanded geometry.
- Patchlist draws with HS+DS bound route through the tessellation prepass pipeline (VS-as-compute +
  HS/DS passthrough + tessellator layout + DS passthrough).
- There is an initial “real GS” path for **point-list and triangle-list draws (`Draw` and `DrawIndexed`)**:
  if the bound GS DXBC can be translated by `crates/aero-d3d11/src/runtime/gs_translate.rs`, the executor
  executes that translated WGSL compute prepass at draw time.
  - Today, GS `v#[]` inputs are populated via vertex pulling:
    - Point-list and triangle-list translated-GS prepass paths prefer feeding `v#[]` from a minimal
      **VS-as-compute** implementation (so the GS observes VS output registers), but this VS execution
      is still a small subset.
    - The executor can fall back to filling `v#[]` directly from IA for strict passthrough VS.
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
suitable for `draw_indexed_indirect`.
It is partially wired into the command executor via the translated-GS prepass paths (`PointList` and
`TriangleList` draws (`Draw` and `DrawIndexed`)); other topologies still fall back to synthetic
expansion.

---

## Current implementation status (AeroGPU command-stream executor)

The AeroGPU D3D10/11 command-stream executor implements GS emulation as a GPU-side **compute expansion
prepass** + **indirect draw** path. It also has an initial “execute guest GS DXBC” path for a small
point-list and triangle-list subset, but it is not yet a complete GS implementation.

There are currently three compute-prepass “modes”:

- **Tessellation emulation (bring-up):** for patchlist draws with HS+DS bound, run a multi-pass
  tessellation prepass (VS-as-compute vertex pulling + HS passthrough + tessellator layout + DS
  passthrough) to expand patches into an indexed triangle list.
- **Real GS execution (supported subset):** translate the guest’s GS DXBC into a WGSL compute shader
  and run it to generate expanded vertices/indices + indirect args.
- **Fallback synthetic expansion (scaffolding):** run `GEOMETRY_PREPASS_CS_WGSL`, which emits synthetic primitives. This
  mode remains useful for adjacency/patchlist scaffolding and for tests that force the compute-prepass
  path without a real GS.

Implemented today:

- **GS/HS/DS bindings**: resource-binding opcodes can target GS (and future HS/DS) binding tables
  without clobbering compute-stage bindings (see “Resource binding model” below). GS can be
  addressed either via `shader_stage = GEOMETRY` (preferred) or via the `stage_ex` compatibility
  encoding; HS/DS require `stage_ex`.
- **Extended `BIND_SHADERS`**: the `BIND_SHADERS` packet can carry `gs/hs/ds` handles via an
  append-only tail (when present, the appended handles are authoritative), and draws route through a
  dedicated “compute prepass” path when any of these stages are bound.
- **Compute→indirect→render pipeline plumbing**: the executor runs a compute prepass to write an
   expanded vertex/index buffer + indirect args, then renders via `draw_indirect` /
   `draw_indexed_indirect` (see `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
  - A built-in WGSL prepass (“synthetic expansion”) is used as a fallback and for bring-up coverage
    tests (see `GEOMETRY_PREPASS_CS_WGSL` / `GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL` in
    `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
  - A translator-backed GS prepass exists for **point-list and triangle-list draws (`Draw` and
    `DrawIndexed`)**: a supported subset of SM4 GS DXBC is translated to WGSL compute and executed
    to produce expanded geometry (see `exec_geometry_shader_prepass_pointlist` and
    `exec_geometry_shader_prepass_trianglelist`).
- **GS DXBC → WGSL compute translation (minimal subset)**:
  - GS DXBC is decoded to SM4 IR and translated to WGSL compute in
    `crates/aero-d3d11/src/runtime/gs_translate.rs` (invoked from `CREATE_SHADER_DXBC` for GS).
  - Strip-cut (`CutVertex` / `RestartStrip`) semantics are validated by deterministic reference
    implementations in `crates/aero-d3d11/src/runtime/strip_to_list.rs`.

Current limitations (high-level):

- Only a small “real GS” path is implemented today:
  - `PointList` and `TriangleList` draws (`Draw` and `DrawIndexed`) can execute translated SM4 GS
    DXBC as the compute prepass when the shader is within the supported translator subset.
  - Other input topologies (line, strip, adjacency) still use the built-in synthetic expansion WGSL
    prepass.
- VS-as-compute feeding for GS inputs is still incomplete:
  - The translated-GS prepass paths prefer a minimal VS-as-compute feeding path so the GS observes
    VS output registers (correct D3D11 semantics), but it is still a small subset (simple VS
    expected).
  - The executor can fall back to IA-fill for strict passthrough VS.
- HS/DS are still scaffolding-only (no real HS/DS DXBC execution yet).

Test pointers:

- End-to-end translated GS execution:
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_pointlist_draw_indexed.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_output_topology_pointlist.rs`
  - `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_emits_triangle.rs`
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

Supported:

- `point` (`D3D11_PRIMITIVE_TOPOLOGY_POINTLIST`)
- `triangle` (`D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST`)

Note: for the current translated-GS prepass paths, the GS `v#[]` inputs are populated via vertex
pulling plus a minimal VS-as-compute feeding path (simple VS subset), with an IA-fill fallback for
strict passthrough VS.

Not yet supported end-to-end:

- `line` input primitives
- strip input topologies (`LINESTRIP`, `TRIANGLESTRIP`)
- adjacency primitives (`lineadj` / `triadj`, i.e. `*_ADJ` topologies)
  - When adjacency support is implemented, the required IA primitive assembly ordering for
    `LINELIST_ADJ`/`LINESTRIP_ADJ` and `TRIANGLELIST_ADJ`/`TRIANGLESTRIP_ADJ` is specified in
    [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md) section 2.1.1b.

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
  - SRV buffers:
    - `ld_raw`
    - `ld_structured`

Supported operand surface (initial):

- temp regs (`r#`) and output regs (`o#`) (note: the translated GS prepass currently stores `o0`
  (position) plus a small subset of `o#` varyings into the expanded vertex buffer; default is `o1`)
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
- **No adjacency (end-to-end)**
  - `lineadj` / `triadj` inputs are not supported by the command-stream executor yet
- **Limited output topology / payload**
  - Output topology is limited to `pointlist`, `linestrip`, and `trianglestrip` (stream 0 only).
    Strip topologies are lowered to list topologies for rendering (`linestrip` → line list,
    `trianglestrip` → triangle list).
  - The expanded-vertex record stores `SV_Position` plus up to 32 `@location(N)` varyings
    (`vec4<f32>` each, indexed by location). The translated GS prepass currently only populates a
    small subset of those varying slots (default: `o1`); other varying slots default to zero.
- **No layered rendering semantics**
  - No `SV_RenderTargetArrayIndex` / `SV_ViewportArrayIndex` style outputs (future work)
- **No fixed-function GS-side rasterizer discard**
  - WebGPU does not expose rasterizer discard; the emulation always runs the render pass
- **WebGL2 backend**
  - WebGL2 has no compute; GS emulation is WebGPU-only (or requires a separate CPU fallback path)

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

Current uses:

- **HS/DS scaffolding:** bring-up work for tessellation uses the same “compute prepass + indirect draw”
  shape, even before HS/DS DXBC execution exists.
- **Adjacency/patchlist scaffolding:** D3D11 topologies that WebGPU cannot represent directly
  (`*_ADJ`, `*_PATCHLIST_*`) can still be routed through the prepass path to exercise plumbing.
- **Tests that force compute-prepass without a real GS:** e.g.
  `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs` binds a dummy GS
  handle and patches the topology to a patchlist value to validate the executor path.

---

## How to test

End-to-end GS emulation (compute prepass executes guest GS DXBC) is covered by:

- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_emits_triangle.rs`

These tests require compute shaders and indirect execution, so they may skip on downlevel backends
(e.g. WebGL2).

Example:

```bash
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_point_to_triangle
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_restart_strip
cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_trianglelist_emits_triangle
```

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
- Expansion-internal buffers (vertex pulling inputs, scratch outputs, counters, indirect args) are
  also internal to the emulation path. In the baseline design they live in `@group(3)` using a
  reserved high binding-number range (starting at `BINDING_BASE_INTERNAL = 256`, defined in
  `crates/aero-d3d11/src/binding_model.rs`) so they do not collide with D3D register bindings (see
  [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md)).
  - Vertex pulling already uses this reserved range so it can be shared across emulation kernels.

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

- **Extra passes**: one or more compute passes (GS itself, and potentially VS-as-compute once brought up)
- **Extra passes**: one or more compute passes (GS itself, and (for some paths) VS-as-compute)
  before the render pass.
- **Intermediate buffers**: VS output + expanded vertex/index buffers + indirect args.
- **Strip→list expansion cost**:
  - `triangle_strip` with `N` emitted vertices produces `(N-2)` triangles, i.e. **`3*(N-2)` list vertices**.
  - `line_strip` with `N` emitted vertices produces `(N-1)` segments, i.e. **`2*(N-1)` list vertices**.

In practice:

- GS-heavy workloads will be bandwidth-bound and should be expected to perform worse than on native D3D.
- The emulation path is best treated as a **compatibility** feature; “fast paths” (pattern-based lowering)
  may still be desirable later for common GS usage patterns.
