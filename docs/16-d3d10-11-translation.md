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

`Dispatch(x, y, z)` maps directly to `dispatchWorkgroups(x, y, z)` once SM5 compute shader translation is available.

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

The chosen bases intentionally carve out disjoint ranges that align with D3D11 per-stage slot
counts:

- Constant buffers: 14 slots (0–13) fit within `[0, 32)`.
- SRVs: 128 slots (0–127) map to `[32, 160)`.
- Samplers: 16 slots (0–15) map to `[160, 176)`.

Examples:

- VS `cb0` → `@group(0) @binding(0)`
- PS `cb0` → `@group(1) @binding(0)` (same slot/binding, different stage group)
- PS `t0`  → `@group(1) @binding(32)`
- PS `s0`  → `@group(1) @binding(160)`

This keeps bindings stable (derived directly from D3D slot indices) without requiring per-shader
rebinding logic.

#### Only resources used by the shader are emitted/bound

D3D11 exposes many binding slots (e.g. 128 SRVs per stage), but typical shaders use only a small
subset. The translator **scans the decoded instruction stream** to determine which resources are
actually referenced, and emits WGSL declarations (and reflection binding metadata) only for those
resources.

The runtime then builds bind group layouts / bind groups from the reflected set of used bindings,
which keeps the implementation within WebGPU’s per-stage binding limits even if the application
binds many unused D3D resources.

#### Future work: UAVs and additional resource types

SM5 UAVs (`u#`), SRV buffers, structured buffers, storage textures, and additional texture
dimensions will need additional WGSL types and likely another disjoint `@binding` range per stage.
This is planned for P2 (compute/UAV-heavy workloads) but is not part of the current implementation.

---

## Geometry Shaders (P1)

WebGPU does not expose a geometry shader stage. To support D3D10/11 GS:

### Strategy A: Pattern-based lowering (fast path)

Recognize common GS patterns and lower them:

- Point sprite expansion (point → quad): expand in VS using instance ID
- Simple extrusion (triangle → triangle strip): expand in VS with vertex pulling

This requires shader analysis rather than perfect ISA coverage, but covers a large fraction of real-world usage.

### Strategy B: General GS emulation (compat path)

1. Run VS as usual, write VS outputs into a storage buffer.
2. Run GS as a compute pass:
   - Read VS output buffer
   - Emit expanded primitives into a new vertex buffer (storage)
   - Write an indirect draw args buffer (vertex count / instance count)
3. Render from the expanded buffer using a “passthrough” vertex shader.

This is conceptually similar to stream output and works even for complex GS, at the cost of extra passes and memory bandwidth.

---

## Tessellation (HS/DS) and Advanced Compute (P2)

WebGPU does not expose fixed-function tessellation. Emulate HS/DS by:

1. Compute pass to generate tessellated vertices and indices into storage buffers.
2. Optional compute pass for patch constant evaluation.
3. Draw using indirect args.

This aligns with the GS emulation strategy and can share the same intermediate-buffer allocator.

Compute shaders (CS) are directly supported once SM5 bytecode translation is complete, with attention to:

- Thread group shared memory mapping
- Atomics mapping (`Interlocked*` ops → WGSL atomics)
- UAV barriers and ordering constraints (see Synchronization)

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
