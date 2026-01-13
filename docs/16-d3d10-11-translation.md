# 16 - Direct3D 10/11 Translation (SM4/SM5 → WebGPU)

## Overview

Windows 7 applications (and some OS compositor paths) use **Direct3D 10/11**. Compared to D3D9, the API surface is more structured and the shader ISA is newer:

- **Shader Model 4.0/5.0** bytecode (DXBC “SHDR/SHEX” tokens)
- **State objects** (blend/depth-stencil/rasterizer/sampler) instead of ad‑hoc render states
- **Constant buffers** (cbuffers) and **resource views** (SRV/UAV/RTV/DSV)
- **More shader stages** (VS/GS/PS in D3D10; +HS/DS/CS in D3D11)

This document specifies the translation layer that maps D3D10/11 concepts onto **WebGPU** primitives while remaining compatible with browser limits and WebGPU’s fixed-function constraints.

Scope ordering:

- **P0 (minimum viable for Win7 apps):** VS/PS SM4/5 + constant buffers + SRV/RTV/DSV + blend/depth/rasterizer + Draw/DrawIndexed (+ instancing)
- **P1:** geometry shaders (limited → full via compute expansion)
- **P2:** compute shaders + tessellation emulation (HS/DS) and UAV-heavy workloads

---

## Pipeline Mapping: D3D11 → WebGPU

### D3D11 fixed pipeline stages

```
Input Assembler  -> VS -> (HS -> Tess -> DS) -> GS -> Rasterizer -> PS -> Output Merger
```

WebGPU has:

- Render pipeline: vertex stage + fragment stage (+ fixed rasterization)
- Compute pipeline: compute stage

So the translation strategy is:

1. **VS + PS** map directly to a WebGPU `GPURenderPipeline`.
2. **CS** maps directly to `GPUComputePipeline`.
3. **GS/HS/DS** require lowering/emulation:
   - Preferred: detect and lower common patterns.
   - General: run as compute to expand primitives into an intermediate vertex buffer, then render from that buffer (see “Geometry/Tessellation” sections).

### Context state → pipeline key

D3D11 binds state incrementally; WebGPU requires most state at pipeline creation time. The translator maintains a **shadow context state**, and on each draw computes a `PipelineKey`:

- Shader bytecode hash (VS/PS + optional GS lowering mode)
- Input layout hash (vertex formats + semantics mapping)
- Primitive topology
- Rasterizer state (cull mode, front face, depth bias)
- Depth/stencil state (+ format)
- Blend state (per render target)
- Render target formats + sample count

Pipelines are cached by key to avoid recreating them every draw.

---

## Resource Model: Objects and Views

D3D10/11 strongly separates **resources** from **views**.

### Resources

- Buffers: `ID3D11Buffer`
- Textures: `ID3D11Texture1D/2D/3D`

### Views

- `SRV` (Shader Resource View): shader-readable
- `UAV` (Unordered Access View): shader read/write
- `RTV` (Render Target View): render pass color attachment
- `DSV` (Depth Stencil View): render pass depth/stencil attachment

WebGPU equivalents:

- Resources: `GPUBuffer`, `GPUTexture`
- Views: `GPUTextureView` + bind group entries

Translation rule of thumb:

- **RTV/DSV** become **attachments** of a `GPURenderPassDescriptor`.
- **SRV** become bind group entries:
  - textures: `texture_2d<f32>` / `texture_2d_array<f32>` / `texture_cube<f32>` etc.
  - buffers: `var<storage, read>` or `var<uniform>` depending on usage.
- **UAV** become `var<storage, read_write>` buffers or storage textures (`texture_storage_2d`).

Important: WebGPU requires declaring usage at resource creation. The translator must create `GPUTexture/GPUBuffer` with a **superset** of needed usage flags based on the D3D bind flags and any view types created later.

---

## Constant Buffers (cbuffers)

D3D10/11 constant buffers are the dominant parameter mechanism for SM4/5.

### D3D semantics

- Up to **14** constant buffer slots per shader stage (0–13) in D3D11.
- Buffers may be updated via `UpdateSubresource` or `Map(D3D11_MAP_WRITE_DISCARD/NO_OVERWRITE)`.
- In SM4/5 bytecode, constant buffer reads are expressed in terms of **16‑byte registers** (`c#` / `cb#[]`).

### WebGPU representation

To avoid layout mismatches between HLSL packing and WGSL’s layout rules, represent each cbuffer as an array of 16‑byte “registers”:

```wgsl
struct Cb0 {
    regs: array<vec4<u32>, CB0_REG_COUNT>;
}
// Bind groups are stage-scoped; this example shows a VS cbuffer (`@group(0)`).
@group(0) @binding(0) var<uniform> cb0: Cb0;
```

Then implement typed loads by bitcasting:

```wgsl
fn cb0_load_f32(reg: u32, lane: u32) -> f32 {
    return bitcast<f32>(cb0.regs[reg][lane]);
}
fn cb0_load_vec4_f32(reg: u32) -> vec4<f32> {
    return bitcast<vec4<f32>>(cb0.regs[reg]);
}
```

This mirrors how SM4/5 actually addresses constants and removes the need to precisely reproduce HLSL packing rules in WGSL structs.

Note: the `@group(N)` index is **stage-scoped** in Aero (VS=0, PS=1, CS=2). The example above shows
the vertex shader group; a pixel shader cbuffer declaration would use `@group(1)` instead (see
“Resource binding mapping”).

### Dynamic updates and renaming

`MAP_WRITE_DISCARD` is naturally implemented as **buffer renaming**:

- Maintain a per-frame uniform ring buffer (256‑byte aligned)
- Each update allocates a new slice and binds via dynamic offsets (native wgpu) or by rebinding a new uniform buffer (JS WebGPU)
- This avoids GPU/CPU hazards without stalling

---

## Input Layouts and Semantics

In D3D11, the input layout defines how vertex buffer elements map to vertex shader inputs by **semantic name + index** (e.g. `POSITION0`, `TEXCOORD1`).

In Aero, the Win7 D3D10/11 UMDs transmit input layouts over the AeroGPU command stream as an opaque blob with magic `"ILAY"` (`AEROGPU_INPUT_LAYOUT_BLOB_MAGIC`). For the D3D10/11 path, the blob encodes `D3D11_INPUT_ELEMENT_DESC`-like data, but with the **semantic name represented as a 32-bit FNV-1a hash of the ASCII-uppercase semantic name** (see `drivers/aerogpu/protocol/aerogpu_cmd.h`).

### Translation approach

1. Parse the vertex shader input signature (`ISGN`) to get semantic list in order.
2. When creating the input layout, build a mapping:
    - `(semantic, index) -> location`
3. Emit WGSL vertex inputs using those locations:

```wgsl
struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv0: vec2<f32>,
}
```

4. Translate the D3D11 `D3D11_INPUT_ELEMENT_DESC` array into WebGPU `VertexBufferLayout`:
    - `Format`: map `DXGI_FORMAT_*` to `VertexFormat`
    - `AlignedByteOffset`: offset
    - `InputSlot`: buffer slot
    - `InputSlotClass/InstanceDataStepRate`: `stepMode = Vertex/Instance`

### WebGPU limits: sparse slot compaction

Unlike D3D11, WebGPU has baseline limits (minimum required by the spec) of:

- **8** vertex buffers
- **16** vertex attributes

D3D11 input layouts can reference up to 32 IA slots (`D3D11_IA_VERTEX_INPUT_RESOURCE_SLOT_COUNT`), and in real workloads these slot indices can be **sparse** (e.g. slots 0 and 15). The translation layer must:

1. Map each ILAY element’s `(semantic hash, semantic index)` to a WGSL `@location` using the **vertex shader input signature** (`ISGN`).
2. **Compact** referenced D3D slot indices into a dense WebGPU slot range (0..N), and maintain a mapping so the correct buffers are bound at draw time.
3. Reject layouts that exceed the WebGPU baseline limits early with clear errors (before pipeline creation).

The input-layout hash must be part of `PipelineKey`, because WebGPU vertex attribute layouts are baked into the pipeline.

---

## State Objects: Blend, Depth/Stencil, Rasterizer, Samplers

### Blend state

D3D11 supports per-render-target blending and write masks. Map to WebGPU `GPUColorTargetState`:

- `BlendEnable` → `blend: Some(BlendState { ... })`
- `RenderTargetWriteMask` → `writeMask`
- `SrcBlend/DestBlend/BlendOp` and alpha equivalents map to WebGPU blend factors/ops.

### Depth/stencil state

Map to WebGPU `DepthStencilState`:

- `DepthEnable` + `DepthWriteMask` + `DepthFunc` map to `depthCompare` and `depthWriteEnabled`
- Stencil: map front/back ops (`StencilFailOp`, `StencilDepthFailOp`, `StencilPassOp`, `StencilFunc`)
- `StencilRef` is dynamic state at draw time

### Rasterizer state

Map to WebGPU `PrimitiveState` + depth bias:

- Cull mode: `D3D11_CULL_*` → `cullMode`
- Front face: `FrontCounterClockwise` → `frontFace`
- Depth bias: map to `depthBias` / `depthBiasSlopeScale` (where supported)
- Scissor enable: dynamic `set_scissor_rect`

Wireframe (`D3D11_FILL_WIREFRAME`) is not directly supported in WebGPU; treat as:

- P0: ignore (render solid), guarded behind a compatibility flag
- Optional: emulate by converting triangle topology to line-list in a preprocessing path

### Samplers

Map D3D sampler state to WebGPU sampler descriptors:

- Filter: `MIN/MAG/MIP` combinations map to `min_filter`, `mag_filter`, `mipmap_filter`
- Address: `WRAP/MIRROR/CLAMP/BORDER` map where possible (WebGPU has no “border color” in the baseline; emulate with clamped sampling + shader-side border handling if required)
- Comparison samplers: map for shadow maps (`comparison: Some(...)`)

---

## Draw and Dispatch Support

### Draw calls

Support the core family:

- `Draw`, `DrawIndexed`
- `DrawInstanced`, `DrawIndexedInstanced`

Mapping to WebGPU:

- Vertex buffers: `setVertexBuffer(slot, buffer, offset)`
- Index buffer: `setIndexBuffer(buffer, format, offset)`
- Draw: `draw(vertexCount, instanceCount, firstVertex, firstInstance)`
- DrawIndexed: `drawIndexed(indexCount, instanceCount, firstIndex, baseVertex, firstInstance)`

`baseVertex` is important for many D3D11 engines; preserve it if the WebGPU implementation supports it.

### Compute

`Dispatch(x, y, z)` maps directly to `dispatchWorkgroups(x, y, z)`.

Compute shader translation is implemented for a subset of SM5 (`cs_5_0`) and is still evolving. The translator currently supports the core thread-ID system values:

- `SV_DispatchThreadID` → `@builtin(global_invocation_id)`
- `SV_GroupID` → `@builtin(workgroup_id)`
- `SV_GroupThreadID` → `@builtin(local_invocation_id)`
- `SV_GroupIndex` → `@builtin(local_invocation_index)`

---

## Shader Translation: DXBC SM4/SM5

### Container parsing

DXBC is a container with typed chunks (notable: `SHEX/SHDR`, `ISGN`, `OSGN`, `RDEF`, `STAT`).

Minimum parsing for SM4/5:

- Verify “DXBC” header and chunk table
- Extract:
  - shader bytecode chunk (`SHDR` for SM4, `SHEX` for SM5)
  - signatures (`ISGN`, `OSGN`, plus `PSGN` if present)
    - Some toolchains emit variant IDs with a trailing `1` (`ISG1`/`OSG1`/`PSG1`); treat these as equivalent signature chunks.
  - resource definitions (`RDEF`) for CB sizes and binding slots

### Instruction decoding model

SM4/5 instructions are tokenized 32-bit words:

- Opcode token (operation + length + flags)
- N operand tokens with modifiers and swizzles
- Optional extended tokens (resource dimension, sample controls, etc.)

Translator pipeline (conceptual):

```
DXBC (SHEX/SHDR) → decode tokens → build SSA-ish IR → WGSL emission
```

Key required features for Win7-era shaders:

- Arithmetic: `add/mul/mad/min/max/rcp/rsq/sqrt`
- Control flow: `if/else/endif`, `loop/endloop`, `break`, `discard`
- Interpolation modifiers for PS inputs
- Texture ops (sample/sample_l/sample_d, ld/`Texture*.Load`)
- Integer ops and bitcasts (used heavily in packing/unpacking)

### Resource binding mapping

SM4/5 binds resources using:

- Constant buffers: `cb#`
- Samplers: `s#`
- SRV textures/buffers: `t#`
- UAVs: `u#` (SM5)

In Aero, the **implemented** SM4/SM5 → WGSL binding model is stage-scoped and deterministic. It is
shared by the shader translator and the command-stream executor (see
`crates/aero-d3d11/src/binding_model.rs` and its use in `crates/aero-d3d11/src/shader_translate.rs`).
This avoids per-shader bespoke layouts: the runtime can bind by `(stage, slot)` without shader-
specific remapping.

#### Bind groups are stage-scoped

Bind groups map 1:1 to D3D11 shader stages:

- `@group(0)`: vertex shader (VS) resources
- `@group(1)`: pixel/fragment shader (PS) resources
- `@group(2)`: compute shader (CS) resources

Why stage-scoped?

- D3D11 resource bindings are tracked per-stage, and stages can be rebound independently.
- Using stage-scoped bind groups lets the runtime keep simple shadow-state and caches per stage:
  rebinding VS resources only invalidates/rebuilds `group(0)`, PS only touches `group(1)`, etc.
- It also keeps pipeline layout assembly straightforward: render pipelines use the VS + PS group
  layouts (0 and 1), compute pipelines use the CS layout (2).

#### Binding numbers use disjoint offset ranges

Within each stage’s bind group, D3D register spaces are mapped into disjoint `@binding` ranges so
`cb#/b#`, `t#`, and `s#` can coexist without collisions. Binding numbers are computed as:

`binding = BINDING_BASE_* + d3d_slot_index`

The base offsets are defined in `crates/aero-d3d11/src/binding_model.rs`:

- `BINDING_BASE_CBUFFER = 0` for constant buffers (`cb#` / `b#`)
- `BINDING_BASE_TEXTURE = 32` for SRV textures (`t#`)
- `BINDING_BASE_SAMPLER = 160` for samplers (`s#`)
- `BINDING_BASE_UAV` for UAV buffers/textures (`u#`, SM5)

The chosen bases intentionally carve out disjoint ranges that align with D3D11 per-stage slot
counts:

- Constant buffers: 14 slots (0–13) fit within `[0, 32)`.
- SRVs: 128 slots (0–127) map to `[32, 160)`.
- Samplers: 16 slots (0–15) map to `[160, 176)`.
- UAVs: `MAX_UAV_SLOTS` slots (0..`MAX_UAV_SLOTS - 1`) map to
  `[BINDING_BASE_UAV, BINDING_BASE_UAV + MAX_UAV_SLOTS)`.

Examples:

- VS `cb0` → `@group(0) @binding(0)`
- PS `cb0` → `@group(1) @binding(0)` (same slot/binding, different stage group)
- PS `t0`  → `@group(1) @binding(32)`
- PS `s0`  → `@group(1) @binding(160)`
- CS `u0`  → `@group(2) @binding(BINDING_BASE_UAV + 0)`

This keeps bindings stable (derived directly from D3D slot indices) without requiring per-shader
rebinding logic.

#### Stage extensions (GS/HS/DS) reuse the compute bind group

WebGPU does not expose native **geometry/hull/domain** stages. Aero emulates these stages by
compiling them to **compute** entry points and inserting compute passes before the final render.

To avoid adding more stage-scoped bind groups (and to stay consistent with
`crates/aero-d3d11/src/binding_model.rs`), **GS/HS/DS shaders use the existing compute group**:

- D3D resources referenced by GS/HS/DS (`b#`, `t#`, `s#`) are declared in WGSL at `@group(2)`
  using the same `@binding` numbers as a normal compute shader.
- The AeroGPU command stream distinguishes “which D3D stage is being bound” using a small
  `stage_ex` extension carried in reserved fields of the resource-binding opcodes (see the ABI
  section below).
- Internal buffers used for vertex pulling and expansion outputs are **not** placed in the
  stage-scoped groups; they live in dedicated “internal” bind groups that are only used by the
  emulation compute pipelines.

#### Only resources used by the shader are emitted/bound

D3D11 exposes many binding slots (e.g. 128 SRVs per stage), but typical shaders use only a small
subset. The translator **scans the decoded instruction stream** to determine which resources are
actually referenced, and emits WGSL declarations (and reflection binding metadata) only for those
resources.

The runtime then builds bind group layouts / bind groups from the reflected set of used bindings,
which keeps the implementation within WebGPU’s per-stage binding limits even if the application
binds many unused D3D resources.

#### Resource types: what’s supported vs. still missing

The current SM5 translation + binding model supports the D3D11-era buffer view types that show up in real compute workloads:

- **SRV buffers** (`t#` with `StructuredBuffer` / `ByteAddressBuffer`-like access) map to
  `var<storage, read>` bindings in WGSL, using the `BINDING_BASE_TEXTURE + slot` binding-number
  scheme.
- **UAV buffers** (`u#` with `RWStructuredBuffer` / `RWByteAddressBuffer`-like access) map to
  `var<storage, read_write>` bindings in WGSL, using `BINDING_BASE_UAV + slot`.

Remaining gaps (planned follow-ups) include:

- **Thread group shared memory** (`groupshared`) mapping to WGSL `var<workgroup>`.
- **Typed UAV textures** (`RWTexture*` / `u#` storage textures) and the required format plumbing.
- **Atomics** (`Interlocked*`) and the necessary WGSL atomic type mapping.
- **Explicit ordering/barriers** (D3D UAV barriers, `GroupMemoryBarrier*` / `DeviceMemoryBarrier*` semantics).

---

## Geometry + Tessellation Emulation (GS/HS/DS) via Compute Expansion (P1/P2)

WebGPU exposes only **vertex** + **fragment** (render) and **compute** pipelines. D3D10/11’s
**GS/HS/DS** stages are therefore implemented by an explicit **compute-expansion pipeline** that:

1. Pulls vertices in compute (using the bound IA state and input layout).
2. Runs the missing D3D stages (VS → HS/DS → GS) as compute kernels, writing expanded vertices (and
    optional indices) into scratch buffers.
3. Issues a final `drawIndirect` / `drawIndexedIndirect` that renders from the generated buffers
    using a small “passthrough” vertex shader + the original pixel shader.

Pattern-based lowering remains an optimization opportunity, but the **compatibility baseline** is
the general compute expansion described below.

### 1) AeroGPU ABI extensions for GS/HS/DS

These changes are designed to be **minor-version, forward-compatible**:
packets grow only by appending new fields, and existing `reserved*` fields are repurposed in a way
that keeps old drivers valid (they still write zeros).

#### 1.1) `stage_ex` in resource-binding opcodes

Many AeroGPU binding packets already carry a `shader_stage` plus a trailing `reserved0` field:

- `AEROGPU_CMD_SET_TEXTURE`
- `AEROGPU_CMD_SET_SAMPLERS`
- `AEROGPU_CMD_SET_CONSTANT_BUFFERS`

For GS/HS/DS we need to bind D3D resources **per stage**, but the D3D11 executor’s stable binding
model only has stage-scoped bind groups for **VS/PS/CS** (`@group(0..2)`). We therefore treat GS/HS/DS
as “compute-like” stages and bind their resources through the existing compute bind group, using a
small `stage_ex` tag carried in the trailing reserved field.

**Definition (conceptual):**

```c
// New: used when binding resources for GS/HS/DS (and optionally compute).
//
// Values match DXBC program-type IDs (`D3D10_SB_PROGRAM_TYPE` / `D3D11_SB_PROGRAM_TYPE`):
//   0 = Pixel, 1 = Vertex, 2 = Geometry, 3 = Hull, 4 = Domain, 5 = Compute.
enum aerogpu_shader_stage_ex {
   AEROGPU_STAGE_EX_PIXEL    = 0,
   AEROGPU_STAGE_EX_VERTEX   = 1,
   AEROGPU_STAGE_EX_GEOMETRY = 2,
   AEROGPU_STAGE_EX_HULL     = 3,
   AEROGPU_STAGE_EX_DOMAIN   = 4,
   AEROGPU_STAGE_EX_COMPUTE  = 5,
};

// Note: in the *binding commands* described here, `stage_ex = 0` is treated as the legacy/default
// “no stage_ex” value (because old guests always write 0 into reserved fields). As a result, the
// DXBC program-type value `0 = Pixel` is not used via this extension; VS/PS continue to bind via
// the legacy `shader_stage` field.

// Example: SET_TEXTURE
struct aerogpu_cmd_set_texture {
   struct aerogpu_cmd_hdr hdr;       // opcode = AEROGPU_CMD_SET_TEXTURE
   uint32_t shader_stage;            // enum aerogpu_shader_stage (0=VS,1=PS,2=CS)
   uint32_t slot;
   aerogpu_handle_t texture;         // 0 = unbind
   uint32_t stage_ex;                // enum aerogpu_shader_stage_ex (was reserved0)
};
```

**Encoding rules:**

- Legacy VS/PS bindings: use the existing `shader_stage` field and write `stage_ex = 0`:
  - VS: `shader_stage = VERTEX`, `stage_ex = 0`
  - PS: `shader_stage = PIXEL`,  `stage_ex = 0`
- For compute, `stage_ex = 0` remains valid legacy encoding:
  - CS: `shader_stage = COMPUTE`, `stage_ex = 0`
- `stage_ex` encoding is enabled by setting `shader_stage = COMPUTE` and `stage_ex != 0`:
  - GS resources: `shader_stage = COMPUTE`, `stage_ex = GEOMETRY` (2)
  - HS resources: `shader_stage = COMPUTE`, `stage_ex = HULL`     (3)
  - DS resources: `shader_stage = COMPUTE`, `stage_ex = DOMAIN`   (4)

The host maintains separate binding tables for CS vs GS/HS/DS so that compute dispatch and
graphics-tess/GS pipelines do not trample each other’s bindings, but they all map to WGSL
`@group(2)` at shader interface level.

#### 1.2) Extended `BIND_SHADERS` packet layout

`AEROGPU_CMD_BIND_SHADERS` is extended by appending `gs/hs/ds` handles after the existing payload.

Compatibility note: the legacy 24-byte packet already has a trailing `reserved0` field. For
forward-compat, the host may interpret `reserved0 != 0` as “a GS is bound” for older guests. When
the extended layout is used, `reserved0` should be set to 0 and the appended `gs/hs/ds` handles are
authoritative.

```c
// Existing prefix (ABI 1.0+):
//   hdr, vs, ps, cs, reserved0
//
// Extension (ABI minor bump): append gs/hs/ds.
struct aerogpu_cmd_bind_shaders {
   struct aerogpu_cmd_hdr hdr;       // opcode = AEROGPU_CMD_BIND_SHADERS
   aerogpu_handle_t vs;              // 0 = unbound
   aerogpu_handle_t ps;              // 0 = unbound
   aerogpu_handle_t cs;              // 0 = unbound
   uint32_t reserved0;               // legacy: may be interpreted as gs when non-zero; extended: should be 0

   // Present when hdr.size_bytes >= 36:
   aerogpu_handle_t gs;              // 0 = unbound
   aerogpu_handle_t hs;              // 0 = unbound
   aerogpu_handle_t ds;              // 0 = unbound
};
```

**Host decoding rule:** if the extension fields are missing, treat `gs/hs/ds` as unbound and (for
compat) optionally treat `reserved0` as `gs`.

#### 1.3) Primitive topology extensions: adjacency + patchlists

`AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY` is extended by adding values to
`enum aerogpu_primitive_topology`:

- **Adjacency topologies** (for GS input):
   - `AEROGPU_TOPOLOGY_LINELIST_ADJ`      = 10
   - `AEROGPU_TOPOLOGY_LINESTRIP_ADJ`     = 11
   - `AEROGPU_TOPOLOGY_TRIANGLELIST_ADJ`  = 12
   - `AEROGPU_TOPOLOGY_TRIANGLESTRIP_ADJ` = 13
- **Patchlists** (for tessellation input), matching D3D11 numbering:
   - `AEROGPU_TOPOLOGY_1_CONTROL_POINT_PATCHLIST`  = 33
   - …
   - `AEROGPU_TOPOLOGY_32_CONTROL_POINT_PATCHLIST` = 64

Notes:

- `AEROGPU_TOPOLOGY_TRIANGLEFAN` remains for the D3D9 path; D3D11 does not emit triangle fans.
- Adjacency/patch topologies always route through the compute-expansion pipeline (even if GS/HS/DS
   are unbound) so behavior is deterministic and validation errors can be surfaced consistently.

### 2) Compute-expansion runtime pipeline

#### 2.1) When the expansion pipeline triggers

A draw uses compute expansion when **any** of the following are true:

- A **GS** shader is bound (`gs != 0`).
- A **HS** or **DS** shader is bound (`hs != 0` or `ds != 0`).
- The IA primitive topology is an **adjacency** topology.
- The IA primitive topology is a **patchlist** topology (33–64).

Otherwise, the existing “direct render pipeline” path is used (VS+PS render pipeline).

#### 2.2) Scratch buffers (logical) and required WebGPU usages

The expansion pipeline uses per-draw (or per-encoder) scratch allocations. These are *logical*
buffers; they may be implemented as separate `wgpu::Buffer`s or as sub-allocations of a larger
transient arena, as long as alignment requirements are respected.

**Alignment requirements:**

- Buffer sizes and offsets must be 4-byte aligned (`wgpu::COPY_BUFFER_ALIGNMENT`) because the
   pipeline uses `copy_buffer_to_buffer` and may clear/initialize buffers with writes.
- If sub-allocating a shared scratch buffer and binding with dynamic offsets, each slice must be
   aligned to `device.limits().min_storage_buffer_offset_alignment` (typically 256).

**Scratch allocations:**

1. **VS-out (`vs_out`)**
    - Purpose: stores vertex shader outputs (control points) for the draw, consumable by HS/GS.
    - Usage: `STORAGE` (written/read by compute).
    - Layout: `array<ExpandedVertex>` (see below).

2. **Tessellation-out (`tess_out_vertices`, `tess_out_indices`)**
    - Purpose: stores post-DS vertices + optional indices.
    - Usage (vertices): `STORAGE | VERTEX`
    - Usage (indices): `STORAGE | INDEX`

3. **GS-out (`gs_out_vertices`, `gs_out_indices`)**
    - Purpose: stores post-GS vertices + indices suitable for final rasterization.
    - Usage (vertices): `STORAGE | VERTEX`
    - Usage (indices): `STORAGE | INDEX`

4. **Indirect args (`indirect_args`)**
    - Purpose: written by compute, consumed by render pass as indirect draw parameters.
    - Usage: `STORAGE | INDIRECT`

5. **Counters (`counters`)**
    - Purpose: atomic counters used during expansion (output vertex count, output index count,
      overflow flags).
    - Usage: `STORAGE` (atomics) and optionally `COPY_SRC` for debugging readback.

**Expanded vertex layout (conceptual):**

For compatibility with the existing signature-driven stage linking, expansion outputs store the
same interface that the pixel shader consumes:

- `pos`: `vec4<f32>` (SV_Position)
- `vN`: `vec4<f32>` for each user varying location `N` used by the pixel shader

The exact `ExpandedVertex` struct is derived from the linked VS/GS/DS → PS signature (a pipeline
already exists for trimming/intersecting varyings; expansion reuses that location set).

#### 2.3) Indirect draw argument formats

The indirect args buffer stores one of the WebGPU-defined structs at offset 0:

```c
// For RenderPass::draw_indirect
struct DrawIndirectArgs {
   uint32_t vertex_count;
   uint32_t instance_count;
   uint32_t first_vertex;
   uint32_t first_instance;
};

// For RenderPass::draw_indexed_indirect
struct DrawIndexedIndirectArgs {
   uint32_t index_count;
   uint32_t instance_count;
   uint32_t first_index;
   int32_t  base_vertex;
   uint32_t first_instance;
};
```

Implementation rules:

- Expansion always writes args at **offset 0** (no multi-draw).
- `first_vertex/first_index/base_vertex` are written as 0 (the expansion output buffers are
   already in the correct space).
- `instance_count` may be preserved for true instancing, but the baseline implementation may
   legally **flatten instancing** by baking instance ID into the expansion passes and setting
   `instance_count = 1` (this keeps output counts independent of instance).

#### 2.4) Pass sequence (per draw)

The following compute passes are inserted before the final render pass. Each pass is dispatched
with an implementation-defined workgroup size chosen by the translator/runtime.

1. **VS (compute variant): vertex pulling + VS execution**
    - Inputs:
      - IA vertex buffers + index buffer (internal bind group)
      - VS resources (still `@group(0)`; this is the existing stage-scoped VS bind group)
      - Draw parameters (first/vertex/index, base vertex, instance info)
    - Output: `vs_out[i] = ExpandedVertex` for each input control point.

2. **Tessellation (optional): HS/DS emulation**
    - Trigger: `hs != 0 || ds != 0 || topology is patchlist`.
    - HS pass:
      - Reads control points from `vs_out`.
      - Writes patch constants + optional HS control points to scratch.
    - Tessellator/DS pass:
      - Generates tessellated domain points and evaluates DS.
      - Writes `tess_out_vertices` (+ `tess_out_indices` if indexed rendering is chosen).
      - Writes `indirect_args` + `counters`.

3. **GS (optional): geometry shader emulation**
    - Trigger: `gs != 0` or adjacency topology.
    - Reads primitive inputs from the previous stage output (`vs_out` for no tessellation,
      otherwise `tess_out_vertices`).
    - Emits vertices/indices to `gs_out_*`, updates counters, then writes final `indirect_args`.

4. **Final render**
    - Uses a render pipeline consisting of:
      - A **passthrough vertex shader** that reads `ExpandedVertex` from the final expansion output
        buffer and outputs the same `@location`s expected by the pixel shader.
      - The original pixel shader.
    - Issues `drawIndirect` or `drawIndexedIndirect` depending on whether an index buffer was
      generated.

### 3) Binding model for emulation kernels

#### 3.1) User (D3D) resources for GS/HS/DS

GS/HS/DS are compiled as compute entry points but keep the normal D3D binding model:

- D3D resources live in `@group(2)` and use the same binding number scheme as compute shaders:
   - `b#` (cbuffers) → `@binding(BINDING_BASE_CBUFFER + slot)`
   - `t#` (SRVs)     → `@binding(BINDING_BASE_TEXTURE + slot)`
   - `s#` (samplers) → `@binding(BINDING_BASE_SAMPLER + slot)`
- Resource-binding opcodes specify the logical stage via `stage_ex` so the runtime can maintain
   separate tables for CS vs GS/HS/DS while still using a single `@group(2)` interface.

#### 3.2) Internal bind groups and reserved bindings

Expansion compute pipelines require additional buffers that are not part of the D3D binding model
(vertex pulling inputs, scratch outputs, counters, indirect args).

These are bound through a dedicated internal bind group to avoid colliding with the stable
`@group(0..2)` layout. The internal group index is reserved as:

- `@group(3)`: **Aero internal expansion bindings** (not visible to guest shaders)

Within `@group(3)`, binding numbers are reserved and stable so the runtime can share common helper
WGSL across VS/GS/HS/DS compute variants:

- `@binding(0)`: `ExpandParams` (uniform/storage; draw parameters + topology info)
- `@binding(1..=8)`: vertex buffers `vb0..vb7` as read-only storage (after slot compaction)
- `@binding(9)`: index buffer (read-only storage; absent → bind dummy)
- `@binding(10)`: `vs_out` (read_write storage)
- `@binding(11)`: `tess_out_vertices` (read_write storage)
- `@binding(12)`: `tess_out_indices` (read_write storage)
- `@binding(13)`: `gs_out_vertices` (read_write storage)
- `@binding(14)`: `gs_out_indices` (read_write storage)
- `@binding(15)`: `indirect_args` (read_write storage)
- `@binding(16)`: `counters` (read_write storage; atomics)

Note: vertex pulling requires reading the guest’s bound vertex/index buffers from compute. The
host must therefore create buffers with `AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER` /
`AEROGPU_RESOURCE_USAGE_INDEX_BUFFER` using WebGPU usages that include `STORAGE` (in addition to
`VERTEX`/`INDEX`), so they can be bound at `@group(3)` as read-only storage buffers.

Buffers bound here must be created with the union of the usages required by the consuming stages
(e.g. `STORAGE | VERTEX` for final vertex output).

### 4) Testing strategy

The goal is to validate the *pipeline plumbing* (ABI parsing, compute expansion, indirect draw,
stage linking) with a small set of deterministic pixel-compare scenes.

**Shader fixtures:**

Add DXBC fixtures alongside existing ones in `crates/aero-d3d11/tests/fixtures/`:

- `gs_*.dxbc` – minimal SM4/SM5 geometry shaders (triangle passthrough, point expansion).
- `hs_*.dxbc` – minimal hull shaders (constant tess factors, pass-through control points).
- `ds_*.dxbc` – minimal domain shaders (simple interpolation to position).

**Pixel-compare tests (Rust):**

Add new `aero-d3d11` executor tests that render to an offscreen RT and compare readback pixels:

- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_*.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_tessellation_*.rs`

Each test should:

1. Upload VS/PS (+ GS/HS/DS) fixtures.
2. Bind topology (including adjacency/patchlist where relevant).
3. Issue a draw that exercises the expansion path.
4. Read back the render target and compare to a tiny reference image (or a simple expected pattern).

When GS support lands, update the existing “ignore GS payloads” robustness test
(`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_ignore.rs`) to reflect the new behavior (GS
is no longer ignored when bound through the extended `BIND_SHADERS` packet).

---

## Synchronization and Queries

D3D11 runtimes rely on GPU/CPU synchronization primitives:

- `ID3D11Query` (event, occlusion, timestamp, pipeline statistics)
- `Flush` and deferred-context command lists

WebGPU equivalents:

- Timestamp/occlusion queries map to `GPUQuerySet` + `resolveQuerySet`
- “Event query” can be implemented as “queue work done” observation:
  - Native: future on queue submission
  - Browser: `queue.onSubmittedWorkDone()`

Important behavioral requirement:

- `GetData` may poll without blocking; implement a non-blocking fast path and only stall when the D3D contract requires it.

---

## Conformance Suite: SM4/5 + D3D11 Features

The translation layer needs a targeted, growing set of reference scenes with **pixel compare**:

### P0 scenes (must pass)

1. **Triangle (VS/PS)**: solid color, no textures
2. **Constant buffer update**: animate color/transform via cbuffer writes
3. **Texture sampling**: 2D texture + sampler state variations (wrap/clamp, linear/point)
4. **Depth test**: two overlapping triangles with depth enabled/disabled
5. **Alpha blending**: premultiplied vs straight alpha comparisons
6. **Instancing**: 100 instances with per-instance matrix buffer

### P1 scenes

7. **Geometry expansion**: point sprites, triangle extrusion

### P2 scenes

8. **Compute blur**: run CS to blur a texture then render it
9. **UAV write**: CS writes to structured buffer; PS reads and visualizes

Each scene should render to an offscreen texture and read back for comparison, using:

- per-pixel absolute tolerance (for floating-point differences)
- SSIM/PSNR fallback for blur-like tests

---

## Performance Sanity Benchmarks

Translation performance regressions are easy to introduce. Maintain a small perf suite:

- Pipeline-cache hit rate (should be high after warm-up)
- CPU time per draw (state hashing + bind group updates)
- Uniform update bandwidth (cbuffer ring allocator)
- GPU frame time for “many draws” microbench (e.g. 5k draws, 100k tris)

---

## Integration Notes

This doc complements:

- `docs/04-graphics-subsystem.md` (overall graphics architecture and DXBC→WGSL flow)
- `docs/graphics/win7-d3d10-11-umd-minimal.md` (Win7 UMD DDI surface: entrypoints, feature levels, swapchain/present expectations)
- `docs/graphics/win7-d3d11ddi-function-tables.md` (Win7 D3D11 `d3d11umddi.h` function-table checklist: REQUIRED vs stubbable for FL10_0 bring-up)
- `docs/12-testing-strategy.md` (how pixel-compare tests fit into CI)
- `docs/15-agent-task-breakdown.md` (task-level execution plan)
