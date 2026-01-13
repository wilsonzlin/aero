# Geometry Shader Emulation (D3D10/11 GS → WebGPU)

WebGPU does **not** expose a geometry shader (GS) stage. Aero’s strategy is to **emulate GS via compute**
by expanding primitives into intermediate vertex/index buffers, then drawing those buffers with a normal
WebGPU render pipeline.

This document describes the **intended/implemented emulation pipeline shape**, its **resource binding
model**, and the **feature subset + limitations** we target for initial GS compatibility.

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

---

## Supported GS feature subset (initial target)

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

Aero supports all three declarations, but the emulation output is rendered as:

- `pointlist` → point list
- `linestrip` → line list (expanded)
- `trianglestrip` → triangle list (expanded)

Only **stream 0** is supported.

### Opcodes / instruction subset

The GS instruction set surface is large; initial emulation focuses on the opcodes required for
“expand a primitive into N primitives” style shaders:

- **Primitive emission**
  - `EmitVertex` (`emit`)
  - `CutVertex` (`cut`)
- **Arithmetic subset**
  - `mov`, `add`, `mul`, `mad`
  - `min`, `max`
  - `dp3`, `dp4`
  - `rcp`, `rsq`

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

Aero uses stage-scoped bind groups for translated SM4/SM5 shaders (see `crates/aero-d3d11/src/binding_model.rs`):

- `@group(0)`: VS resources
- `@group(1)`: PS resources
- `@group(2)`: CS resources

For GS emulation (and future HS/DS emulation):

- GS/HS/DS run as **compute** entry points but bind *their D3D resources* through a reserved internal
  bind group so they never trample CS bindings:
  - `@group(3)` for GS/HS/DS resources (selected via the `stage_ex` ABI extension).
- Expansion-internal buffers (vertex pulling inputs, scratch outputs, counters, indirect args) also
  live in `@group(3)` but use a reserved binding-number range (see
  [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md)).

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
("vertex pulling"), and intermediate outputs must be written to scratch buffers.

These bindings are **internal** to the emulation path; they are not visible to the D3D binding model
and are specified in the compute-expansion section of
[`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md) (including the reserved internal
binding-number range).

### AeroGPU command stream note: `stage_ex`

The AeroGPU command stream historically only had `Vertex/Pixel/Compute` stage enums.
To bind resources for additional D3D stages (GS/HS/DS) without breaking ABI, the protocol supports a
“stage_ex” extension (see `emulator/protocol/aerogpu/aerogpu_cmd.rs`):

- For certain binding commands (`SET_TEXTURE`, `SET_SAMPLERS`, `SET_CONSTANT_BUFFERS`,
  `SET_SHADER_RESOURCE_BUFFERS`, `SET_UNORDERED_ACCESS_BUFFERS`, `SET_SHADER_CONSTANTS_F`):
  - set `shader_stage = COMPUTE` (legacy value `2`)
  - use `reserved0` as a small `stage_ex` tag (values match DXBC program types):
    - `0 = Pixel`, `1 = Vertex`, `2 = Geometry`, `3 = Hull`, `4 = Domain`, `5 = Compute`
  - for compatibility, `reserved0 == 0` is treated as “legacy compute” in binding packets; any
    non-zero `stage_ex` value selects an extended stage (GS/HS/DS).

This keeps older hosts/guests forward-compatible while letting newer versions express GS-stage bindings.

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
