# Geometry Shader Emulation (D3D10/11 GS → WebGPU)

WebGPU does **not** expose a geometry shader (GS) stage. Aero’s strategy is to **emulate GS via compute**
by expanding primitives into intermediate vertex/index buffers, then drawing those buffers with a normal
WebGPU render pipeline.

This document describes:

- what is **implemented today** (command-stream plumbing, binding model, compute-prepass scaffolding + current limitations), and
- the **next steps** (wire up GS DXBC translation + bytecode execution, then expand opcode/topology coverage, system values, etc).

> Related: [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md) (high-level D3D10/11→WebGPU mapping).

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

### Why we expand strips into lists

D3D GS outputs are typically declared as `line_strip` or `triangle_strip`, and can use `CutVertex`
to terminate the current strip and start a new one.

For simplicity and portability, Aero’s emulation expands strips into lists:

- `line_strip` → **line list** (each new vertex after the first emits one line segment)
- `triangle_strip` → **triangle list** (each new vertex after the first two emits one triangle)

This avoids needing to generate restart indices and keeps the draw stage in the most widely-supported
primitive topologies.

Note: there is an in-progress GS→WGSL compute translator (`crates/aero-d3d11/src/runtime/gs_translate.rs`)
which currently focuses on `triangle_strip` output; it is not yet wired into the command executor.

---

## Current implementation status (AeroGPU command-stream executor)

The AeroGPU D3D10/11 command-stream executor has the plumbing needed to emulate GS/HS/DS via a GPU-side
compute expansion prepass, but it is not yet a full “execute guest GS DXBC” implementation.

Implemented today:

- **GS/HS/DS bindings**: resource-binding opcodes can target GS (and future HS/DS) binding tables
  without clobbering compute-stage bindings (see “Resource binding model” below). GS can be
  addressed either via `shader_stage = GEOMETRY` (preferred) or via the `stage_ex` compatibility
  encoding; HS/DS require `stage_ex`.
- **Extended `BIND_SHADERS`**: the `BIND_SHADERS` packet can carry `gs/hs/ds` handles, and draws route
  through a dedicated “compute prepass” path when any of these stages are bound.
- **Compute→indirect→render pipeline plumbing**: the executor runs a compute prepass to write an
  expanded vertex/index buffer + indirect args, then renders via `draw_indirect` /
  `draw_indexed_indirect` (see `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).

Current limitation (important):

- The draw-time compute prepass is currently a **placeholder** (see `GEOMETRY_PREPASS_CS_WGSL`) that
  emits fixed triangles and does **not** execute the guest’s GS/HS/DS DXBC yet.
- DXBC payloads that parse as GS/HS/DS are accepted at `CREATE_SHADER_DXBC` time, but are currently
  stored as stub WGSL modules (no real execution yet).

Test pointers:

- Placeholder compute-prepass smoke: `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs`
- In-progress GS translator unit tests (not wired into the executor yet): `crates/aero-d3d11/tests/gs_translate.rs`
- Future end-to-end semantics (once wired): `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs`

---

## Target GS feature subset (initial)

### Input primitive types

Supported (non-adjacency) GS input primitive declarations:

- `point`
- `line`
- `triangle`

Not supported:

- `lineadj` / `triadj` (adjacency primitives)

### Output topology / stream types

D3D geometry shaders only declare one of:

- `pointlist`
- `linestrip`
- `trianglestrip`

The initial GS translator/execution target focuses on `trianglestrip` output, expanded into a
**triangle list** for rendering.

Planned follow-ups include:

- `pointlist` output (rendered as a point list)
- `linestrip` output (expanded into a line list)

Only **stream 0** is supported.

### Opcodes / instruction subset

The GS instruction set surface is large; initial emulation focuses on the opcodes required for
“expand a primitive into N primitives” style shaders.

- **Primitive emission**
  - `EmitVertex` (`emit`)
  - `CutVertex` (`cut`)
- **Arithmetic subset**
  - `mov`, `add` (plus immediate constants and basic register operands)

Anything outside this subset is expected to be rejected by translation (or will remain unsupported
until implemented).

---

## Current limitations / non-goals

Geometry shader emulation is intentionally *not* a full D3D11 GS implementation in its first version.
Known limitations include:

- **No multi-stream output**
  - No `EmitStream` / `CutStream`
  - No simultaneous multiple output stream declarations
- **No stream-out (SO / transform feedback)**
  - GS output cannot be captured into D3D stream-out buffers
- **No adjacency**
  - `lineadj` / `triadj` inputs are not supported
- **No layered rendering semantics**
  - No `SV_RenderTargetArrayIndex` / `SV_ViewportArrayIndex` style outputs (future work)
- **No fixed-function GS-side rasterizer discard**
  - WebGPU does not expose rasterizer discard; the emulation always runs the render pass
- **WebGL2 backend**
  - WebGL2 has no compute; GS emulation is WebGPU-only (or requires a separate CPU fallback path)

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

- `@group(3)` for GS/HS/DS resources (selected via the `stage_ex` ABI extension).
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
 both “stage_ex” bindings (low `@binding` ranges) and vertex pulling/expansion internal bindings
 (`@binding >= BINDING_BASE_INTERNAL`) within `@group(3)` (keeping the bind group count within the
 WebGPU baseline of 4).

### AeroGPU command stream note: `stage_ex`

The AeroGPU command stream has legacy `shader_stage` enums that mirror WebGPU (VS/PS/CS) and also
includes an explicit Geometry stage (`shader_stage = GEOMETRY`).

To support additional D3D programmable stages (HS/DS) without breaking ABI, some packets support a
“stage_ex” extension that overloads the `reserved0` field when `shader_stage == COMPUTE` (see
`emulator/protocol/aerogpu/aerogpu_cmd.rs`).

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

- **Extra passes**: at least two compute passes (VS + GS) before the render pass.
- **Intermediate buffers**: VS output + expanded vertex/index buffers + indirect args.
- **Strip→list expansion cost**:
  - `triangle_strip` with `N` emitted vertices produces `(N-2)` triangles, i.e. **`3*(N-2)` list vertices**.
  - `line_strip` with `N` emitted vertices produces `(N-1)` segments, i.e. **`2*(N-1)` list vertices**.

In practice:

- GS-heavy workloads will be bandwidth-bound and should be expected to perform worse than on native D3D.
- The emulation path is best treated as a **compatibility** feature; “fast paths” (pattern-based lowering)
  may still be desirable later for common GS usage patterns.
