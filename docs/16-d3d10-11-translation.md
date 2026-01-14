# 16 - Direct3D 10/11 Translation (SM4/SM5 → WebGPU)

## Overview

Windows 7 applications (and some OS compositor paths) use **Direct3D 10/11**. Compared to D3D9, the API surface is more structured and the shader ISA is newer:

- **Shader Model 4.0/5.0** bytecode (DXBC “SHDR/SHEX” tokens)
- **State objects** (blend/depth-stencil/rasterizer/sampler) instead of ad‑hoc render states
- **Constant buffers** (cbuffers) and **resource views** (SRV/UAV/RTV/DSV)
- **More shader stages** (VS/GS/PS in D3D10; +HS/DS/CS in D3D11)

This document specifies the translation layer that maps D3D10/11 concepts onto **WebGPU** primitives while remaining compatible with browser limits and WebGPU’s fixed-function constraints.

> Protocol note: **AeroGPU ABI 1.3+** (minor bump) introduces the guest↔host command-stream
> extensions needed for D3D10/11 parity:
> - GS support via the explicit `shader_stage = GEOMETRY` encoding (and `stage_ex` extensions for HS/DS;
>   GS also has a `stage_ex` compatibility encoding)
> - extended `BIND_SHADERS` encoding
> - additional primitive topologies (beyond the original D3D9-oriented subset)

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

Note: the `@group(N)` index is **stage-scoped** in Aero for user (D3D) resources. The stable
stage→group mapping is:

- `@group(0)`: vertex shader (VS)
- `@group(1)`: pixel/fragment shader (PS)
- `@group(2)`: compute shader (CS)
- `@group(3)`: reserved internal / emulation group (GS/HS/DS resources + vertex pulling / expansion scratch)

The example above shows the vertex shader group; a pixel shader cbuffer declaration would use
`@group(1)` instead (see “Resource binding mapping”).

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

Note: The authoritative token bitfield layout is defined in the Windows SDK headers
`d3d10tokenizedprogramformat.h` / `d3d11tokenizedprogramformat.h` (e.g. instruction length lives in
bits 24..30 of the opcode token, operand type in bits 12..19, saturate in bit 13, etc.).

Note: SM4/SM5 also distinguishes **ordered** vs **unordered** floating-point comparisons in the
tokenized program format. For `setp` (predicate set) the `*_U` suffix in
`D3D10_SB_INSTRUCTION_COMPARISON` / `D3D11_SB_INSTRUCTION_COMPARISON` means **unordered**
(NaN-aware): the comparison is true if either operand is NaN. This is separate from unsigned integer
comparisons, which are encoded via distinct opcodes (e.g. `ult`/`uge`).

At the time of writing, Aero's checked-in SM4/SM5 fixtures and unit tests use a simplified
token encoding for bring-up (see `crates/aero-d3d11/src/sm4/opcode.rs`), so not all of the official
DXBC bitfield layout is reflected in the current decoder implementation yet.

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

Bind groups are stage-scoped and mostly map 1:1 to D3D11 shader stages:

- `@group(0)`: vertex shader (VS) resources
- `@group(1)`: pixel/fragment shader (PS) resources
- `@group(2)`: compute shader (CS) resources
- `@group(3)`: reserved internal / emulation group:
  - geometry/hull/domain (GS/HS/DS) resources (tracked separately from CS to avoid clobbering)
  - internal emulation helpers (vertex pulling, expansion scratch, counters, indirect args)

Why stage-scoped?

- D3D11 resource bindings are tracked per-stage, and stages can be rebound independently.
- Using stage-scoped bind groups lets the runtime keep simple shadow-state and caches per stage:
  rebinding VS resources only invalidates/rebuilds `group(0)`, PS only touches `group(1)`, etc.
- It also keeps pipeline layout assembly straightforward:
  - Render pipelines use the VS + PS group layouts (0 and 1).
  - Compute pipelines use the CS layout (2).
  - Emulation pipelines (GS/HS/DS expansion) may additionally use `@group(3)`, which carries both
    the GS/HS/DS D3D resources and a reserved `@binding` range for internal emulation buffers.

#### Binding numbers use disjoint offset ranges

Within each stage’s bind group, D3D register spaces are mapped into disjoint `@binding` ranges so
`cb#/b#`, `t#`, and `s#` can coexist without collisions. Binding numbers are computed as:

`binding = BINDING_BASE_* + d3d_slot_index`

The base offsets are defined in `crates/aero-d3d11/src/binding_model.rs`:

- `BINDING_BASE_CBUFFER = 0` for constant buffers (`cb#` / `b#`)
- `BINDING_BASE_TEXTURE = 32` for SRV textures (`t#`)
- `BINDING_BASE_SAMPLER = 160` for samplers (`s#`)
- `BINDING_BASE_UAV = 176` for UAV buffers/textures (`u#`, SM5), i.e.
  `BINDING_BASE_SAMPLER + MAX_SAMPLER_SLOTS`
- `BINDING_BASE_INTERNAL = 256` for internal emulation bindings (vertex pulling, expansion scratch,
  counters, indirect args)

The chosen bases intentionally carve out disjoint ranges that align with D3D11 per-stage slot
counts:

- Constant buffers: 14 slots (0–13) fit within `[0, 32)`.
- SRVs: 128 slots (0–127) map to `[32, 160)`.
- Samplers: 16 slots (0–15) map to `[160, 176)`.
- UAVs: `MAX_UAV_SLOTS = 8` slots (0..`MAX_UAV_SLOTS - 1`, i.e. `u0..u7`) map to
  `[BINDING_BASE_UAV, BINDING_BASE_UAV + MAX_UAV_SLOTS)`.

Examples:

- VS `cb0` → `@group(0) @binding(0)`
- PS `cb0` → `@group(1) @binding(0)` (same slot/binding, different stage group)
- PS `t0`  → `@group(1) @binding(32)`
- PS `s0`  → `@group(1) @binding(160)`
- CS `u0`  → `@group(2) @binding(BINDING_BASE_UAV + 0)`

This keeps bindings stable (derived directly from D3D slot indices) without requiring per-shader
rebinding logic.

#### Stage extensions (GS/HS/DS) use the reserved extended-stage bind group (`@group(3)`)

WebGPU does not expose native **geometry/hull/domain** stages. Aero emulates these stages by
compiling them to **compute** entry points and inserting compute passes before the final render.

To keep the user (D3D) binding model within WebGPU’s baseline bind group count (4), and to ensure
GS/HS/DS binds never trample compute-shader state, Aero reserves a fourth stage-scoped bind group:

- D3D resources referenced by GS/HS/DS (`b#`, `t#`, `s#`, and optionally `u#`) are declared in WGSL at
  `@group(3)` using the same `@binding` number scheme (`BINDING_BASE_* + slot`) as other stages.
- The AeroGPU command stream distinguishes which binding table is being updated (CS vs GS/HS/DS)
  either via the direct `shader_stage = GEOMETRY` encoding (preferred for GS), or via the `stage_ex`
  extension carried in reserved fields of the resource-binding opcodes when `shader_stage = COMPUTE`
  (required for HS/DS; optional GS compatibility). See the ABI section below.
- Expansion-specific internal buffers (vertex pulling inputs, scratch outputs, counters, indirect
  args) are internal to the compute-expansion pipeline. In the baseline design these live alongside
  GS/HS/DS resources in the reserved extended-stage bind group (`@group(3)`) using a reserved
  binding-number range starting at `BINDING_BASE_INTERNAL = 256` (see “Internal bindings” below).
  - Note: the current executor’s compute-prepass still uses an ad-hoc bind group layout for some
    output buffers, but vertex pulling already uses the reserved internal binding range so it can
    coexist with GS/HS/DS bindings.
  - Implementation detail: the in-tree vertex pulling WGSL uses `@group(3)` (see
    `VERTEX_PULLING_GROUP` in `crates/aero-d3d11/src/runtime/vertex_pulling.rs`) and pads pipeline
    layouts with empty groups so indices line up. Because the binding numbers are already in the
    reserved `>= 256` range, it can safely coexist with GS/HS/DS D3D bindings in the same group
    without collisions.
  See [`docs/graphics/geometry-shader-emulation.md`](./graphics/geometry-shader-emulation.md).

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

- **Typed UAV textures**: partially supported (write-only `RWTexture2D`-style stores via
  `texture_storage_2d`), but limited to a small set of DXGI formats (see
  `StorageTextureFormat` in `crates/aero-d3d11/src/shader_translate.rs`).

- **Atomics**: partially supported (e.g. basic `InterlockedAdd` on UAV buffers), but the full
  `Interlocked*` family and more complex UAV/structured patterns are still missing.

- **Explicit ordering/barriers**: partially supported via SM5 `sync` (`GroupMemoryBarrier*` /
  `DeviceMemoryBarrier*`).
  - Full barriers (`*WithGroupSync`) map onto WGSL barrier built-ins based on the requested memory
    ordering semantics:
    - TGSM/workgroup ordering: `workgroupBarrier()`
    - UAV/storage ordering: `storageBarrier()`
    - All-memory ordering: both (emitted as `storageBarrier(); workgroupBarrier();`)
    Barriers that appear after potentially conditional returns are rejected (to avoid generating
    WGSL that can deadlock when not all invocations reach the barrier).
  - Fence-only variants (no thread-group sync) do **not** have a perfect WGSL/WebGPU mapping today:
    WGSL `storageBarrier()` is validated/implemented as a workgroup-level barrier in WebGPU/Naga and
    therefore comes with uniform-control-flow requirements that are not necessarily present in the
    original DXBC semantics. The translator therefore conservatively rejects fence-only `sync` in
    potentially divergent control flow (e.g. inside structured `if`/`loop`/`switch`, after conditional
    returns, or under instruction predication).
  - `sync` instructions that set unknown `D3D11_SB_SYNC_FLAGS` bits are rejected (to avoid silently
    dropping ordering semantics).

---

## Geometry + Tessellation Emulation (GS/HS/DS) via Compute Expansion (P1/P2)

WebGPU exposes only **vertex** + **fragment** (render) and **compute** pipelines. D3D10/11’s
**GS/HS/DS** stages are therefore implemented by an explicit **compute-expansion pipeline** that:

> Quick overview + current limitations: see
>
> - [`docs/graphics/geometry-shader-emulation.md`](./graphics/geometry-shader-emulation.md)
> - [`docs/graphics/tessellation-emulation.md`](./graphics/tessellation-emulation.md)

1. Pulls vertices in compute (using the bound IA state and input layout).
2. Runs the missing D3D stages (VS → HS/DS → GS) as compute kernels, writing expanded vertices (and
    optional indices) into scratch buffers.
3. Issues a final `drawIndirect` / `drawIndexedIndirect` that renders from the generated buffers
    using a small “passthrough” vertex shader + the original pixel shader.

Pattern-based lowering remains an optimization opportunity, but the **compatibility baseline** is
the general compute expansion described below.

### P1/P2 tiers and limitations (what we implement first vs later)

This section defines the *shape* of the emulation pipeline (compute expansion + passthrough VS),
but not every D3D feature is required on day one. We explicitly tier the work so implementers can
bring up correctness incrementally:

- **P1 (geometry shader)**
  - **P1a (fast path):** pattern-based lowering for a few common GS uses (e.g. point sprites / quad
    expansion) without any compute passes. This is optional and can be added later.
  - **P1b (baseline):** general GS emulation via compute expansion as specified below.
  - Initial P1b limitations (explicit):
    - no stream-out / transform feedback (SO targets are unsupported),
    - only stream 0 (no `EmitStream` / `CutStream` / `SV_StreamID`),
    - draw instancing (`instance_count > 1`) in the **translated-GS prepass** is still bring-up only:
      the in-tree implementation currently assumes `instance_count == 1` when executing translated GS
      DXBC. (GS instancing via `[instance(n)]` / `SV_GSInstanceID` is supported.)
    - adjacency input primitives (`*_ADJ` topologies / `lineadj`/`triadj`):
      - The required IA primitive assembly ordering is specified in section 2.1.1b.
      - The in-tree translated-GS execution path is currently wired for list topologies:
        - non-adj: `POINTLIST`, `LINELIST`, `TRIANGLELIST`
        - adj (list): `LINELIST_ADJ`, `TRIANGLELIST_ADJ`
      - Adjacency strip topologies (`LINESTRIP_ADJ`, `TRIANGLESTRIP_ADJ`) are not yet supported
        end-to-end; the runtime MUST NOT silently reinterpret them as non-adjacency primitives.
    - **IA primitive restart** (indexed strip topologies) is supported in the direct draw path:
      - D3D11 encodes strip restart in the index buffer as `0xFFFF` (u16) / `0xFFFFFFFF` (u32).
      - Indexed `LINESTRIP`/`TRIANGLESTRIP` draws are supported (native WebGPU primitive restart where
        available).
      - On backends where native primitive restart is unreliable (notably wgpu GL), the executor
        emulates restart by converting the strip into a list index buffer (see
        `crates/aero-d3d11/src/runtime/strip_to_list.rs` and
        `exec_draw_indexed_strip_restart_emulated` in
        `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
      - This keeps restart handling within a single draw call and preserves `SV_PrimitiveID` in the
        pixel shader (see
        `crates/aero-d3d11/tests/aerogpu_cmd_primitive_restart_primitive_id.rs`).
      - Compute-side primitive assembly for strip input topologies (including adjacency strips) is
        future work.
    - output strip topologies are expanded into lists (`line_strip` → `line_list`, `triangle_strip` → `triangle_list`),
    - no layered rendering system values (`SV_RenderTargetArrayIndex`, `SV_ViewportArrayIndex`),
    - output ordering:
      - in a fully-parallel append/emit implementation, ordering is implementation-defined unless we
        add a deterministic prefix-sum/compaction mode (affects strict `SV_PrimitiveID`
        expectations), but
      - initial bring-up can choose a deterministic single-thread GS execution mode (looping over
        primitives in-order) at the cost of performance.

- **P2 (tessellation: HS/DS)**
  - Tessellation is staged by supported **domain / partitioning**:
    - **P2a:** `domain("tri")` with integer partitioning, conservative clamping of tess factors.
    - **P2b:** `domain("quad")` with integer partitioning.
    - **P2c:** `domain("isoline")`.
    - **P2d:** fractional partitioning + crack-free edge rules.
  - Initial P2a limitations (explicit):
    - only integer partitioning (no `fractional_even`/`fractional_odd`/`pow2`),
    - tess factor handling is conservative and may ignore per-edge variation (see “Tessellation sizing”),
    - only tess factors (`SV_TessFactor` / `SV_InsideTessFactor`) are plumbed through as patch-constant
      data initially (no additional user patch constants),
    - HS control-point pass may be treated as a pass-through for bring-up (HS does not modify control
      points), and
    - GS-after-tessellation (HS/DS → GS) is supported only when the GS emulation path can consume the
      indexed tess output (future work; not required for initial P2 bring-up).

#### Capabilities required

GS/HS/DS emulation requires the underlying WebGPU device/backend to support:

- compute pipelines (and storage buffers/atomics)
- indirect draws (to consume the generated draw args)

#### How to debug GS issues

Geometry shader failures are often “silent” (nothing draws) because the expansion pass can legally
emit zero primitives. Recommended debug workflow:

1. Use the in-repo DXBC dump tools (`dxbc_dump` or `sm4_dump`) on the DXBC to confirm:
   - declared input primitive type and output topology
   - `maxvertexcount`
   - input/output signatures (semantics + component masks)
2. Confirm the command stream is binding the expected GS/HS/DS shader handles (via the extended
    `BIND_SHADERS` packet, or via the legacy `reserved0` GS slot) and that stage-specific resources
    are being populated (via `shader_stage = GEOMETRY` bindings or the `stage_ex` encoding).
3. Enable wgpu/WebGPU validation and inspect errors around the expansion compute pass and the
   subsequent indirect draw (binding visibility, usage flags, and out-of-bounds writes are common
   root causes).

### 1) AeroGPU ABI extensions for GS/HS/DS

These changes are designed to be **minor-version, forward-compatible**:
packets grow only by appending new fields, and existing `reserved*` fields are repurposed in a way
that keeps old drivers valid (old drivers typically write zeros into reserved fields).

For the `stage_ex` extension specifically, hosts must additionally gate interpretation by the
command stream ABI minor (introduced in ABI 1.3 / minor=3): command streams older than that may not
reliably zero `reserved0`, so `reserved0` must be treated as reserved/ignored even when
`shader_stage == COMPUTE`.

#### 1.1) `stage_ex` in resource-binding opcodes

The legacy `enum aerogpu_shader_stage` is extended with `GEOMETRY = 3`, so GS resources can be
bound directly using `shader_stage = GEOMETRY` with `reserved0 = 0`.

To represent additional D3D11 stages (HS/DS) without extending the legacy stage enum (and as an
optional GS compatibility encoding), many binding packets overload their trailing `reserved0` field
as a `stage_ex` selector when `shader_stage == COMPUTE`.

Many AeroGPU binding packets already carry a `shader_stage` plus a trailing `reserved0` field:

- `AEROGPU_CMD_SET_TEXTURE`
- `AEROGPU_CMD_SET_SAMPLERS`
- `AEROGPU_CMD_SET_CONSTANT_BUFFERS`
- `AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS` (SRV buffers)
- `AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS` (UAV buffers)
- `AEROGPU_CMD_SET_SHADER_CONSTANTS_F` (legacy D3D9 constants)

Additionally, `AEROGPU_CMD_CREATE_SHADER_DXBC` carries a `stage` field plus `reserved0`, which is
used to represent HS/DS creation without extending the legacy stage enum.

For GS/HS/DS we need to bind D3D resources **per stage**, but the D3D11 executor’s stable binding
model only has stage-scoped bind groups for **VS/PS/CS** (`@group(0..2)`). We therefore treat GS/HS/DS
as “compute-like” stages but route their bindings into a reserved extended-stage bind group
(`@group(3)`), using either:

- the direct `shader_stage = GEOMETRY` encoding (preferred for GS), or
- a small `stage_ex` tag carried in the trailing reserved field (required for HS/DS).

This is implemented in the emulator-side protocol mirror as `AerogpuShaderStageEx` + helpers
`encode_stage_ex`/`decode_stage_ex` (plus ABI-minor-gated variants `decode_stage_ex_gated` /
`resolve_shader_stage_with_ex_gated`; see `emulator/protocol/aerogpu/aerogpu_cmd.rs`).

**CREATE_SHADER_DXBC encoding:**

- Invariant: if `stage != COMPUTE`, `reserved0` must be 0 and is ignored.
- Legacy compute (ABI 1.0+): `stage = COMPUTE` and `reserved0 = 0`.
- Extended stage selector: set `stage = COMPUTE` and store a non-zero `stage_ex` in `reserved0`:
  - GS: `reserved0 = GEOMETRY` (2) (alternative to legacy `stage = GEOMETRY`)
  - HS: `reserved0 = HULL` (3)
  - DS: `reserved0 = DOMAIN` (4)

Hosts should treat unknown non-zero `reserved0` values as invalid for now (reserved for future
stages/extensions).

**Packet layouts that carry `stage_ex` (normative summary)**

For all of the following packets:

- the struct is `#pragma pack(push, 1)` packed,
- `hdr.size_bytes` must include the header + any trailing payload arrays, and
- `reserved0` is interpreted as `stage_ex` **only** when the command stream ABI minor is >= 3
  (ABI 1.3+) and the packet targets the legacy Compute stage:
  - For packets with an explicit legacy `shader_stage`/`stage` field: only when that field equals
    `COMPUTE`.
  - For `DISPATCH` (which is always compute): the trailing `reserved0` u32 is treated as `stage_ex`.
  (`reserved0 == 0` means "no stage_ex override"/legacy compute).

| Packet | Packed struct header | Trailing payload |
|---|---|---|
| `CREATE_SHADER_DXBC` | `aerogpu_cmd_create_shader_dxbc { shader_handle, stage, dxbc_size_bytes, reserved0(stage_ex) }` | `dxbc_bytes[dxbc_size_bytes]` |
| `SET_TEXTURE` | `aerogpu_cmd_set_texture { shader_stage, slot, texture, reserved0(stage_ex) }` | none |
| `SET_SAMPLERS` | `aerogpu_cmd_set_samplers { shader_stage, start_slot, sampler_count, reserved0(stage_ex) }` | `aerogpu_handle_t samplers[sampler_count]` |
| `SET_CONSTANT_BUFFERS` | `aerogpu_cmd_set_constant_buffers { shader_stage, start_slot, buffer_count, reserved0(stage_ex) }` | `aerogpu_constant_buffer_binding bindings[buffer_count]` |
| `SET_SHADER_RESOURCE_BUFFERS` | `aerogpu_cmd_set_shader_resource_buffers { shader_stage, start_slot, buffer_count, reserved0(stage_ex) }` | `aerogpu_shader_resource_buffer_binding bindings[buffer_count]` |
| `SET_UNORDERED_ACCESS_BUFFERS` | `aerogpu_cmd_set_unordered_access_buffers { shader_stage, start_slot, uav_count, reserved0(stage_ex) }` | `aerogpu_unordered_access_buffer_binding bindings[uav_count]` |
| `SET_SHADER_CONSTANTS_F` | `aerogpu_cmd_set_shader_constants_f { stage, start_register, vec4_count, reserved0(stage_ex) }` | `float data[vec4_count * 4]` |
| `DISPATCH` | `aerogpu_cmd_dispatch { group_count_x, group_count_y, group_count_z, reserved0(stage_ex) }` | none |

Implementers should copy the exact struct definitions (and sizes) from
`drivers/aerogpu/protocol/aerogpu_cmd.h` (source of truth). The table above exists so readers can
understand the extension pattern without having to jump to the header.

**Definition (numeric values match DXBC program type values):**

```c
// New: used when binding resources for GS/HS/DS (and optionally compute).
//
// Values match DXBC program-type IDs (`D3D10_SB_PROGRAM_TYPE` / `D3D11_SB_PROGRAM_TYPE`):
//   Pixel=0, Vertex=1, Geometry=2, Hull=3, Domain=4, Compute=5.
enum aerogpu_shader_stage_ex {
   // 0 = no stage_ex override (legacy Compute).
   AEROGPU_SHADER_STAGE_EX_NONE     = 0,
   AEROGPU_SHADER_STAGE_EX_GEOMETRY = 2,
   AEROGPU_SHADER_STAGE_EX_HULL     = 3,
   AEROGPU_SHADER_STAGE_EX_DOMAIN   = 4,
   // Optional alias for Compute. Writers should emit 0 for Compute to preserve legacy semantics.
   AEROGPU_SHADER_STAGE_EX_COMPUTE  = 5,
};

// Note: in the *binding commands* described here, `stage_ex = 0` is treated as the legacy/default
// “no stage_ex” value (because old guests always write 0 into reserved fields). As a result, the
// DXBC program-type value `0 = Pixel` is not used via this extension; VS/PS continue to bind via
// the legacy `shader_stage` field. (Equivalently: `0` is reserved for legacy compute packets.)

// Example: SET_TEXTURE
struct aerogpu_cmd_set_texture {
   struct aerogpu_cmd_hdr hdr;       // opcode = AEROGPU_CMD_SET_TEXTURE
   uint32_t shader_stage;            // enum aerogpu_shader_stage (0=VS,1=PS,2=CS,3=GS legacy)
   uint32_t slot;
   aerogpu_handle_t texture;         // 0 = unbind
   uint32_t stage_ex;                // enum aerogpu_shader_stage_ex (was reserved0)
};
```

**Encoding rules:**

- The `stage_ex` overload is only active when `shader_stage == COMPUTE` (2). For other
  `shader_stage` values, `stage_ex` must be 0.
- Legacy VS/PS bindings: use the existing `shader_stage` field and write `stage_ex = 0`:
  - VS: `shader_stage = VERTEX`, `stage_ex = 0`
  - PS: `shader_stage = PIXEL`,  `stage_ex = 0`
- For compute, `stage_ex = 0` remains valid legacy encoding (old guests write zeros for reserved
  fields):
  - CS: `shader_stage = COMPUTE`, `stage_ex = 0`
- Robustness note: some older/broken command writers may incorrectly use the DXBC program-type value
  for compute (`stage_ex = AEROGPU_SHADER_STAGE_EX_COMPUTE = 5`) instead of the reserved `0`
  sentinel in binding packets. Hosts MAY treat `stage_ex = 5` as equivalent to `0` for compute-stage
  bindings for best-effort compatibility (the in-tree executor does this).
- `stage_ex` encoding is enabled by setting `shader_stage = COMPUTE` and a non-zero `stage_ex`:
  - GS resources: `shader_stage = COMPUTE`, `stage_ex = GEOMETRY` (2)
  - HS resources: `shader_stage = COMPUTE`, `stage_ex = HULL`     (3)
  - DS resources: `shader_stage = COMPUTE`, `stage_ex = DOMAIN`   (4)
  - `stage_ex = 1` (Vertex DXBC program type) is invalid and must not be used; VS must be encoded
    via the legacy `shader_stage` field.
  - `stage_ex = 5` (Compute DXBC program type) may be accepted by decoders as an alias for legacy
    Compute, but writers should emit `stage_ex = 0`.

**GS note:** because `enum aerogpu_shader_stage` includes `GEOMETRY = 3`, GS resource bindings may be
encoded either as:

- `shader_stage = GEOMETRY`, `stage_ex = 0` (direct/legacy GS encoding), or
- `shader_stage = COMPUTE`, `stage_ex = GEOMETRY` (uniform “stage_ex” encoding shared with HS/DS).

Implementations should accept both. Producers should prefer the direct `shader_stage = GEOMETRY`
encoding for GS for better backward compatibility; HS/DS require the `stage_ex` encoding. The
`stage_ex` encoding may still be used for GS for compatibility with components that only implement
the extension.

The host maintains separate binding tables for CS vs GS/HS/DS so that compute dispatch and
graphics-tess/GS pipelines do not trample each other’s bindings. At the WGSL interface level this
maps to distinct bind groups:

- CS uses `@group(2)`.
- GS/HS/DS use `@group(3)`.

#### 1.2) Extended `BIND_SHADERS` packet layout

`AEROGPU_CMD_BIND_SHADERS` is extended by appending `gs/hs/ds` handles after the existing payload.

Compatibility note: the legacy 24-byte packet already has a trailing `reserved0` field. Some older
streams/hosts repurpose this field as the **geometry shader (GS) handle**:

- Legacy 24-byte packet (`hdr.size_bytes == 24`):
  - `reserved0 == 0` → GS unbound
  - `reserved0 != 0` → GS is bound to handle `reserved0`

The extended layout appends `{gs, hs, ds}` as explicit trailing fields.

When using the extended layout, producers MAY still populate `reserved0` with the GS handle as a
redundant compatibility copy (so tooling/hosts that only understand the legacy 24-byte packet can
still observe a bound GS). If they do so, it SHOULD match the appended `gs` field.

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
   uint32_t reserved0;               // legacy GS handle (only when hdr.size_bytes==24; may mirror gs)

   // Present when hdr.size_bytes >= 36:
   aerogpu_handle_t gs;              // 0 = unbound
   aerogpu_handle_t hs;              // 0 = unbound
   aerogpu_handle_t ds;              // 0 = unbound
};
```

**Host decoding rule:**

- If the extension fields are present (`hdr.size_bytes >= 36`), use the appended `gs/hs/ds`.
- Otherwise, treat `gs = reserved0` and `hs = ds = 0`.

#### 1.3) Primitive topology extensions: adjacency + patchlists

`AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY` is extended by adding values to
`enum aerogpu_primitive_topology`:

- **Adjacency topologies** (for GS input):
   - `AEROGPU_TOPOLOGY_LINELIST_ADJ`      = 10
   - `AEROGPU_TOPOLOGY_LINESTRIP_ADJ`     = 11
   - `AEROGPU_TOPOLOGY_TRIANGLELIST_ADJ`  = 12
   - `AEROGPU_TOPOLOGY_TRIANGLESTRIP_ADJ` = 13
- **Patchlists** (for tessellation input), matching D3D11 numbering:
   - `AEROGPU_TOPOLOGY_PATCHLIST_1`  = 33
   - …
   - `AEROGPU_TOPOLOGY_PATCHLIST_32` = 64

Notes:

- `AEROGPU_TOPOLOGY_TRIANGLEFAN` remains for the D3D9 path; D3D11 does not emit triangle fans.
- Adjacency/patch topologies are not directly expressible in WebGPU render pipelines and therefore
  require the compute-expansion pipeline (GS/HS/DS emulation).
  - Until the relevant emulation kernels exist, the runtime MUST NOT silently reinterpret the
    topology as a non-adjacency/list/strip topology (that would silently misrender).
  - Acceptable behaviors are:
    - route the draw through the emulation path (executing a supported GS when present; otherwise
      using bring-up scaffolding / synthetic expansion for plumbing tests), or
    - reject the draw with a clear error.
  - Implementation note: the in-tree D3D11 executor currently routes adjacency/patchlist topologies
    through the emulation path to exercise render-pass splitting + indirect draw plumbing even when
    GS/HS/DS are unbound.

### 2) Compute-expansion runtime pipeline

#### 2.1) When the expansion pipeline triggers

A draw uses compute expansion when **any** of the following are true:

- A **GS** shader is bound (`gs != 0`).
- A **HS** or **DS** shader is bound (`hs != 0` or `ds != 0`).

In the fully-general design, adjacency and patchlist topologies also route through this path even
if GS/HS/DS are unbound (so the runtime can surface deterministic validation/errors and implement
fixed-function tessellation semantics). Today, adjacency and patchlist topologies are accepted by
`SET_PRIMITIVE_TOPOLOGY` and are routed through the emulation path:

- adjacency-list topologies (`*_LIST_ADJ`) can execute the translated GS prepass when a compatible GS
  is bound, and
- patchlist-only and unsupported topology cases (e.g. strip and strip-adjacency) may still use
  bring-up scaffolding / synthetic expansion.

The runtime MUST NOT silently reinterpret these topologies as non-adjacency/list/strip topologies;
acceptable behaviors are to route through the emulation/scaffolding path or to reject with a clear
error (see topology extension notes above).

Otherwise, the existing “direct render pipeline” path is used (VS+PS render pipeline).

#### 2.1.1) Derived counts (vertex invocations, primitive count, patch count)

Compute expansion needs a few derived counts that are normally implicit in a fixed-function input
assembler. These must be computed identically in both the runtime (for sizing) and the compute
kernels (for bounds checks).

Let:

- `draw_kind` be `Draw` or `DrawIndexed`.
- `input_vertex_invocations = (draw_kind == DrawIndexed) ? index_count : vertex_count`
  - Note: this is the number of *vertex shader invocations* in the expansion path. It is **not**
    the number of unique vertices (there is no vertex cache).

For non-patch topologies, the number of *input primitives per instance* (`input_prim_count`) is:

| Topology | Vertices consumed | Primitive count |
|---|---:|---:|
| `POINTLIST` | 1 / prim | `input_vertex_invocations` |
| `LINELIST` | 2 / prim | `input_vertex_invocations / 2` |
| `LINESTRIP` | N | `max(0, input_vertex_invocations - 1)` |
| `TRIANGLELIST` | 3 / prim | `input_vertex_invocations / 3` |
| `TRIANGLESTRIP` | N | `max(0, input_vertex_invocations - 2)` |
| `LINELIST_ADJ` | 4 / prim | `input_vertex_invocations / 4` |
| `LINESTRIP_ADJ` | `prim + 3` | `max(0, input_vertex_invocations.saturating_sub(3))` |
| `TRIANGLELIST_ADJ` | 6 / prim | `input_vertex_invocations / 6` |
| `TRIANGLESTRIP_ADJ` | `2*prim + 4` | `max(0, (input_vertex_invocations.saturating_sub(4)) / 2)` |

Rules:

- Any leftover vertices that don’t form a full primitive are ignored (matching D3D behavior).
- For instanced draws, the input primitive stream is replicated per instance. Many sizing and
  dispatch formulas therefore use:
  - `input_prim_count_total = input_prim_count * instance_count`
  - and flatten `(instance_id, primitive_id_in_instance)` into a single `prim_id` in
    `0..input_prim_count_total` for compute expansion (see GS pass sequence and `gs_inputs` packing).
- `*_ADJ` topologies require adjacency-aware primitive assembly (and typically a GS that declares
  `lineadj`/`triadj`). The in-tree translated-GS prepass supports adjacency **list** topologies
  (`LINELIST_ADJ`, `TRIANGLELIST_ADJ`); strip-adjacency (`*_STRIP_ADJ`) remains future work.
  The runtime MUST NOT reinterpret adjacency topologies as non-adjacency; acceptable behaviors are
  to route through the emulation path (executing a supported GS when possible; otherwise using
  scaffolding/synthetic expansion) or to reject the draw with a clear error.
- **Primitive restart (indexed strip topologies):** for indexed `LINESTRIP`/`TRIANGLESTRIP` (and
  their adjacency variants `LINESTRIP_ADJ`/`TRIANGLESTRIP_ADJ`), D3D11 uses a special index value to
  restart the strip (`0xFFFF` for u16 indices, `0xFFFFFFFF` for u32 indices).
  - Primitive restart affects both the effective primitive count and the `primitive_id → vertices`
    mapping (the simple formulas above assume a single uninterrupted strip).
  - In the direct draw path, the in-tree executors handle restart outside the shader stage:
    - `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs` emulates restart on wgpu GL by
      converting strip indices into a list index buffer (`exec_draw_indexed_strip_restart_emulated`,
      using `crates/aero-d3d11/src/runtime/strip_to_list.rs`). Other backends rely on native
      primitive restart.
    - `crates/aero-d3d11/src/runtime/execute.rs` (`D3D11Runtime`) emulates restart on wgpu GL by
      converting indexed strips into list indices (see `expand_indexed_strip_to_list` and
      `crates/aero-d3d11/src/runtime/strip_to_list.rs`) and using a list-topology pipeline variant.
      Other backends rely on native primitive restart.
  - For compute-expansion passes that need to *consume* strip topologies (GS/HS/DS), implementations
    either need a restart-aware assembly stage or must preprocess strips into list form before
    packing `gs_inputs`. This becomes especially important for `*_ADJ` strip topologies, which are
    not yet supported end-to-end.

For patchlist topologies:

- `control_points = topology - 32` (since `PATCHLIST_1 = 33`, …, `PATCHLIST_32 = 64`)
- `patch_count = input_vertex_invocations / control_points`
  - leftover indices/vertices are ignored.
- For instanced draws, the patch stream is replicated per instance:
  - `patch_count_total = patch_count * instance_count`
  - and `patch_instance_id` ranges `0..patch_count_total`.

Patchlist draws require HS+DS for correct tessellation semantics. If HS/DS are unbound, the runtime
MUST NOT silently reinterpret the topology as a non-patch topology; it should either route through
the emulation path (if scaffolding is present) or reject the draw with a clear error.

#### 2.1.1b) IA primitive assembly (mapping from `primitive_id` → vertex invocations)

For GS emulation (and for any fixed-function logic that depends on IA primitive structure), the
expansion pipeline needs a deterministic way to map an input `primitive_id` into the **vertex shader
invocations** that form that primitive.

Note: in this section, `primitive_id` is the **per-instance** primitive index in
`0..input_prim_count`. If the implementation flattens instancing into a single `prim_id` dimension
(`prim_id in 0..(input_prim_count * instance_count)`), derive:

- `instance_id = prim_id / input_prim_count`
- `primitive_id = prim_id % input_prim_count`

Key point: the expansion path models the input stream in terms of **vertex invocations**
(`input_vertex_invocations`), not unique vertices. There is no vertex cache: for indexed draws the
VS may run multiple times for the same underlying vertex index (legal in D3D; results should be
equivalent).

Let:

- `vinv` be a vertex invocation ID in `0..input_vertex_invocations`.
- `instance_id` be `0..instance_count`.
- `vs_out_index = instance_id * input_vertex_invocations + vinv`.

Then, for non-adjacency topologies, the assembled primitive vertices (as `vinv` IDs) are:

| Topology | Primitive `p` consumes `vinv` IDs |
|---|---|
| `POINTLIST` | `(p)` |
| `LINELIST` | `(2p, 2p + 1)` |
| `LINESTRIP` | `(p, p + 1)` |
| `TRIANGLELIST` | `(3p, 3p + 1, 3p + 2)` |
| `TRIANGLESTRIP` | parity-dependent (see below) |

**Triangle strip winding (important)**

D3D’s `TRIANGLESTRIP` assembly alternates vertex order to maintain consistent winding. For primitive
`p`:

- if `p` is even: `(p, p + 1, p + 2)`
- if `p` is odd:  `(p + 1, p, p + 2)` (swap the first two)

This ordering is what a GS that declares `triangle` input should observe when consuming a
triangle strip.

**Patchlists (tessellation input)**

For patchlists, HS/DS consume **patches** rather than `primitive_id`-assembled points/lines/tris.
The patch input mapping is:

- `patch_vertex_base = patch_id * control_points`
- control point `cp` in `0..control_points` corresponds to `vinv = patch_vertex_base + cp`
- `vs_out_index = instance_id * input_vertex_invocations + vinv`

**Adjacency topologies (`lineadj` / `triadj`)**

Adjacency topologies provide extra “neighbor” vertices to the GS. The runtime MUST validate that the
bound GS declares a matching input primitive:

- `*_ADJ` line topologies (`LINELIST_ADJ`, `LINESTRIP_ADJ`) require a GS input primitive of
  `lineadj`.
- `*_ADJ` triangle topologies (`TRIANGLELIST_ADJ`, `TRIANGLESTRIP_ADJ`) require a GS input primitive
  of `triadj`.

If the topology and GS input primitive disagree, the draw is invalid and should fail with a clear
error (do not silently reinterpret).

**Line adjacency (`lineadj`) vertex order (normative)**

For `lineadj` GS inputs, each input primitive has 4 vertices in this order:

```
input[0] = adjacent vertex before the line start
input[1] = line start vertex
input[2] = line end vertex
input[3] = adjacent vertex after the line end
```

Mapping from `primitive_id = p` to `vinv` IDs:

| Topology | Primitive `p` consumes `vinv` IDs |
|---|---|
| `LINELIST_ADJ` | `(4p + 0, 4p + 1, 4p + 2, 4p + 3)` |
| `LINESTRIP_ADJ` | `(p + 0, p + 1, p + 2, p + 3)` |

**Triangle adjacency (`triadj`) vertex order (normative)**

For `triadj` GS inputs, each input primitive has 6 vertices in this order:

```
input[0] = tri vertex 0
input[1] = adjacent vertex for edge (0, 2)
input[2] = tri vertex 1
input[3] = adjacent vertex for edge (2, 4)
input[4] = tri vertex 2
input[5] = adjacent vertex for edge (4, 0)
```

Equivalently: even indices (0,2,4) are the triangle vertices in order around the triangle, and odd
indices (1,3,5) are the opposite/adjacent vertices for the three edges.

Mapping from `primitive_id = p` to `vinv` IDs:

| Topology | Primitive `p` consumes `vinv` IDs |
|---|---|
| `TRIANGLELIST_ADJ` | `(6p + 0, 6p + 1, 6p + 2, 6p + 3, 6p + 4, 6p + 5)` |
| `TRIANGLESTRIP_ADJ` | parity-dependent (see below) |

**Triangle strip adjacency winding (important)**

`TRIANGLESTRIP_ADJ` follows triangle-strip winding rules: odd primitives swap their first two
triangle vertices to maintain consistent winding.

Let `base = 2p`.

- if `p` is even: `(base + 0, base + 1, base + 2, base + 3, base + 4, base + 5)`
- if `p` is odd:  `(base + 2, base + 1, base + 0, base + 5, base + 4, base + 3)`

This yields triangle vertices at indices 0/2/4 in the correct strip-winding order for both even and
odd primitives, while preserving the `triadj` adjacency-edge mapping described above.

Implementation note: adjacency primitive assembly is specified here for implementability. The in-tree
AeroGPU command-stream executor executes translated GS DXBC over adjacency **list** input topologies
(`LINELIST_ADJ`, `TRIANGLELIST_ADJ`) end-to-end today (see
`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_linelistadj_emits_triangle.rs` and
`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelistadj_emits_triangle.rs`). Adjacency
strip topologies (`LINESTRIP_ADJ`, `TRIANGLESTRIP_ADJ`) are not yet supported for translated GS DXBC
execution; these currently fall back to the generic placeholder/synthetic expansion path.

#### 2.1.1c) GS input register payload layout (optional; matches in-tree `gs_translate`)

The compute-emulated GS needs access to the previous stage’s per-vertex outputs for each input
primitive. One implementation strategy (used by the in-tree SM4 GS→WGSL translator in
`crates/aero-d3d11/src/runtime/gs_translate.rs`) is to pre-pack the GS inputs into a dense “register
file” buffer that the translated GS code can index with a simple formula.

**Layout**

`gs_inputs` is a storage buffer of `vec4<f32>` registers.

Note: `gs_inputs` is conceptually **read-only**, but the current translated-GS pipeline declares it
as `var<storage, read_write>` so it can share a backing scratch buffer with other read/write
allocations without tripping WebGPU’s per-buffer STORAGE_READ vs STORAGE_READ_WRITE exclusivity
rules.

In the baseline internal binding scheme, this buffer is bound at:

- `@group(3) @binding(276)` (optional; only required when using this packed-input GS path).

Implementation note: the current in-tree GS→WGSL translator (`runtime/gs_translate.rs`) uses a fixed
internal bind group at `@group(0)` (separate from the stage-scoped `@group(3)` GS binding model):

- `@binding(0)`: expanded vertices (read_write)
- `@binding(1)`: expanded indices (read_write)
- `@binding(2)`: indirect args (`DrawIndexedIndirectArgs`, read_write)
- `@binding(3)`: atomic counters (read_write)
- `@binding(4)`: uniform params (uniform)
- `@binding(5)`: `gs_inputs` (`storage, read_write`; conceptually read-only)
 
If the GS references D3D resources (constant buffers, SRVs, samplers), the translator also declares
them in the executor’s shared internal/emulation bind group:
  
- `@group(3) @binding(BINDING_BASE_CBUFFER + slot)`: `cb#[]` uniform buffers
- `@group(3) @binding(BINDING_BASE_TEXTURE + slot)`: `t#` SRV textures/buffers
- `@group(3) @binding(BINDING_BASE_SAMPLER + slot)`: `s#` samplers

When wiring that translator into the executor, either adapt its declarations to the baseline
internal scheme, or bind a separate internal group(0) for the GS pass (the current translated-GS
prepass paths for `PointList`, `LineList`, `TriangleList`, `LineListAdj`, and `TriangleListAdj` draws
use the separate group(0) approach; see section 2.2.1).

Example declaration:

```wgsl
struct Vec4F32Buffer { data: array<vec4<f32>>; }
// Note: `read_write` is used for compatibility with wgpu's per-buffer storage usage rules when
// sharing scratch buffers; this buffer is conceptually read-only.
@group(G) @binding(B) var<storage, read_write> gs_inputs: Vec4F32Buffer;
```

Flattened indexing (normative, matches `gs_translate`):

```
idx = ((prim_id * GS_INPUT_VERTS_PER_PRIM + vertex_in_prim) * GS_INPUT_REG_COUNT + reg)
```

Where:

- `prim_id` is the GS `SV_PrimitiveID` over the expanded input stream.
  - In the baseline compute-expansion design we **flatten instancing** into `prim_id` so packing and
    deterministic ordering are simple:
    - `prim_id` ranges `0..(input_prim_count * instance_count)`
    - `instance_id = prim_id / input_prim_count`
    - `primitive_id_in_instance = prim_id % input_prim_count`
  - Alternative: preserve instancing in the GS phase by treating `primitive_id_in_instance` as the
    `prim_id` dimension and carrying `instance_id` separately; this requires a different dispatch
    shape and a different `gs_inputs` packing scheme.
- `vertex_in_prim` is the vertex index within the input primitive (`0..GS_INPUT_VERTS_PER_PRIM`),
  where `GS_INPUT_VERTS_PER_PRIM` depends on the GS declared input primitive:
  - point: 1
  - line: 2
  - triangle: 3
  - lineadj: 4
  - triadj: 6
- `reg` is the input register index within the GS (`v#` register index).
- `GS_INPUT_REG_COUNT` is the number of input registers packed per vertex. A safe value is:
  - `max_used_v_reg + 1`, where `max_used_v_reg` is the maximum `v#` register index referenced by
    the GS instruction stream.

**Populating `gs_inputs`**

To populate `gs_inputs`, the runtime must:

1. assemble input primitives (`primitive_id` → vertex invocations) according to the IA topology (see
   section 2.1.1b), and
2. for each vertex in the assembled primitive, populate the required `v#[]` input registers:
   - Target design: copy the required output registers from the previous stage’s output register
     buffer (`vs_out_regs` or DS output regs) into the packed `gs_inputs`.
   - Current in-tree implementation note: the point-list, line-list, triangle-list, and adjacency-list
     (`LINELIST_ADJ`/`TRIANGLELIST_ADJ`) translated-GS prepass paths in
     `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs` populate `gs_inputs` from **VS outputs**
     via vertex pulling plus a minimal VS-as-compute feeding path (simple SM4 subset), with a guarded
      IA-fill fallback:
       - If VS-as-compute translation fails, the executor only falls back to direct IA-fill when the
         VS is a strict passthrough (or `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is set to force the
         IA-fill fallback for debugging; may misrender). Otherwise the draw fails with a clear error.
       - Extending translated-GS execution to additional IA topologies (strip and strip-adjacency) requires
         the input-fill pass to implement the primitive assembly rules in section 2.1.1b, including
         primitive restart for indexed strips.
       - Note: translated GS prepass execution currently assumes `instance_count == 1` (draw instancing
         is not implemented yet for this path).

Note: an alternative design is to have the translated GS code read directly from the upstream stage
register buffer, eliminating the extra packing step. This is a follow-up optimization.

#### 2.1.2) Tessellation sizing (P2a: tri domain, integer partitioning; conservative)

Tessellation output sizes depend on **tess factors** produced by the HS patch-constant function, so
the runtime must define a sizing policy that is both safe (no OOB) and practical.

For initial bring-up (P2a), we use a conservative per-patch tess level `T` and generate a simple
uniform tessellation pattern:

- Clamp tess factors to the D3D11 max range (robustness):
  - If the factor is `NaN`/`Inf`, treat it as `0.0`.
  - `tf = clamp(tf, 0.0, 64.0)`
- Convert to integer segment counts:
  - `seg = round(tf)` (as `u32`)
- Choose a single tessellation level for the patch:
  - `T = max(edge_seg[0..2], inside_seg[0])` then `T = clamp(T, 1, 64)`

This deliberately ignores per-edge variation and crack-free edge rules (P2d work). It is
deterministic and implementable with minimal fixed-function emulation.

Implementation note: in the compute-emulation pipeline, the HS patch-constant pass writes the raw
tess factors into `tess_patch_constants[patch_instance_id]` (where
`patch_instance_id = instance_id * patch_count + patch_id`). The tessellator/DS pass then derives
`T` from those stored factors (either in a dedicated layout pass or as part of the tessellator/DS
kernel).

For a tri-domain patch tessellated at level `T`:

- Domain vertices per patch: `V_patch = (T + 1) * (T + 2) / 2`
- Triangles per patch: `P_patch = T * T`
- Indices per patch (triangle list): `I_patch = 3 * P_patch`

**Tri-domain grid enumeration (P2a; concrete)**

To make DS evaluation and index generation implementable, we define a concrete enumeration of the
uniform tri-domain grid.

Let `T >= 1` be the patch tessellation level.

**Vertex enumeration**

Vertices are enumerated in “rows” `r = 0..T`, each containing `L_r = (T - r + 1)` vertices, with
column `c = 0..(L_r - 1)`.

The barycentric `SV_DomainLocation` for vertex `(r, c)` is:

- `u = f32(r) / f32(T)`
- `v = f32(c) / f32(T)`
- `w = 1.0 - u - v`

The linear index of `(r, c)` within the patch vertex array is:

```
row_start(r) = r * (T + 1) - (r * (r - 1)) / 2
idx(r, c) = row_start(r) + c
```

**Inverse mapping (linear id → `(r, c)`)**

Some implementations will dispatch DS evaluation with a single linear `domain_vertex_id` in
`0..V_patch`. To derive `(r, c)` from `domain_vertex_id` deterministically (without floating-point
math), use the row-lengths:

```wgsl
// Precondition: 0 <= domain_vertex_id < V_patch.
var id: u32 = domain_vertex_id;
var r: u32 = 0u;
loop {
  let row_len: u32 = T - r + 1u;
  if (id < row_len) { break; }
  id -= row_len;
  r += 1u;
}
let c: u32 = id;
```

Then compute `SV_DomainLocation` using the formulas above.

**Triangle list indices**

For each row `r = 0..(T - 1)`:

- Emit the “up” triangle for `c = 0..(T - r - 1)`:
  - `a = idx(r, c)`
  - `b = idx(r + 1, c)`
  - `c0 = idx(r, c + 1)`
  - triangle = `(a, b, c0)`
- Emit the “down” triangle for `c = 0..(T - r - 2)`:
  - `a = idx(r + 1, c)`
  - `b = idx(r + 1, c + 1)`
  - `c0 = idx(r, c + 1)`
  - triangle = `(a, b, c0)`

This produces exactly `T*T` triangles and `(T+1)(T+2)/2` vertices.

Winding:

- For `outputtopology("triangle_ccw")`, use the triangle order above.
- For `outputtopology("triangle_cw")`, swap `b` and `c0` for each triangle (reverse winding).

**Writing into global buffers**

The indices above are *patch-local* vertex indices. When writing into the shared output buffers:

- `tess_out_vertices[base_vertex + local_vertex_id] = ...`
- `tess_out_indices[base_index + local_index_id] = base_vertex + patch_local_vertex_id`

Where `base_vertex` / `base_index` are read from `tess_patch_state[patch_instance_id]`.

Total sizes for a draw (pre-GS) are:

- `V_total = patch_count * instance_count * V_patch`
- `I_total = patch_count * instance_count * I_patch`

Final output topology (when no GS is bound):

- tri/quad domains → triangle list (`wgpu::PrimitiveTopology::TriangleList`)
- isoline domain → line list (`wgpu::PrimitiveTopology::LineList`)

For P2a tri-domain, the render pass therefore uses `drawIndexedIndirect` with triangle-list
topology.

The runtime may:

- allocate buffers for the worst case (`T = 64`) and rely on overflow-detection to skip draws when
  scratch runs out, or
- allocate dynamically based on per-patch `T` (recommended; requires per-patch allocation state,
  e.g. `tess_patch_state`).

#### 2.1.2b) Tessellation sizing (P2b: quad domain, integer partitioning; conservative)

For `domain("quad")` we need two tessellation levels: one along U and one along V.

As with P2a, we use a conservative, uniform policy based on the HS patch-constant tess factors:

- Clamp tess factors:
  - If the factor is `NaN`/`Inf`, treat it as `0.0`.
  - `tf = clamp(tf, 0.0, 64.0)`
- Convert to integer segment counts:
  - `seg = round(tf)` (as `u32`)
- Choose conservative U/V tessellation levels:
  - `T_u = max(edge_seg[0], edge_seg[2], inside_seg[0])` then `T_u = clamp(T_u, 1, 64)`
  - `T_v = max(edge_seg[1], edge_seg[3], inside_seg[1])` then `T_v = clamp(T_v, 1, 64)`

Store:

- `tess_patch_state[patch_instance_id].tess_level_u = T_u`
- `tess_patch_state[patch_instance_id].tess_level_v = T_v`

For a quad-domain patch tessellated at `(T_u, T_v)`:

- Domain vertices per patch: `V_patch = (T_u + 1) * (T_v + 1)`
- Triangles per patch (triangle list): `P_patch = 2 * T_u * T_v`
- Indices per patch (triangle list): `I_patch = 3 * P_patch = 6 * T_u * T_v`

**Quad-domain grid enumeration (P2b; concrete)**

Let `T_u >= 1` and `T_v >= 1`.

**Vertex enumeration**

Vertices are enumerated in rows `r = 0..T_v`, each containing `T_u + 1` vertices with column
`c = 0..T_u`.

The `SV_DomainLocation` for vertex `(r, c)` is:

- `u = f32(c) / f32(T_u)`
- `v = f32(r) / f32(T_v)`

The linear index of `(r, c)` within the patch vertex array is:

```
idx(r, c) = r * (T_u + 1) + c
```

Inverse mapping (linear id → `(r, c)`):

```
r = domain_vertex_id / (T_u + 1)
c = domain_vertex_id % (T_u + 1)
```

**Triangle list indices**

For each cell `r = 0..(T_v - 1)`, `c = 0..(T_u - 1)`:

- `a = idx(r, c)`
- `b = idx(r, c + 1)`
- `c0 = idx(r + 1, c)`
- `d = idx(r + 1, c + 1)`
- Emit triangles:
  - `(a, b, c0)`
  - `(b, d, c0)`

Winding:

- For `outputtopology("triangle_ccw")`, use the triangle order above.
- For `outputtopology("triangle_cw")`, swap the last two indices of each triangle (reverse winding).

Writing into global buffers uses the same `{base_vertex, base_index}` offsetting rule as tri-domain.

Total sizes for a draw (pre-GS) are:

- `V_total = patch_count * instance_count * V_patch`
- `I_total = patch_count * instance_count * I_patch`

Final output topology (when no GS is bound) is triangle list, and the render pass uses
`drawIndexedIndirect`.

#### 2.1.2c) Tessellation layout pass (deterministic prefix-sum; recommended)

In D3D11, tess factors are produced by the HS patch-constant function, so the output vertex/index
counts are not known until after HS has executed.

A practical bring-up strategy is to insert a dedicated **layout pass** between HS patch-constant and
DS evaluation that:

1. derives a per-patch tessellation level from the stored tess factors,
2. computes per-patch vertex/index counts,
3. computes prefix-sum base offsets (`base_vertex`/`base_index`) so patches write into disjoint
   ranges, and
4. writes the final indirect-draw args for the post-tessellation draw.

This avoids atomics and makes output ordering deterministic (useful for debugging and for strict
system-value expectations). One simple, deterministic implementation is:

- dispatch a single thread (`@workgroup_size(1)` and `global_invocation_id.x == 0`),
- loop over `patch_instance_id` in ascending order (`0..(patch_count * instance_count)`),
- maintain running totals `{total_vertices, total_indices}`,
- write `tess_patch_state[patch_instance_id] = {tess_level_u/v, base_*, *_count}` and increment totals.

**Capacity/overflow policy (required)**

Because the scratch buffers have finite capacity (`params.out_max_vertices`, `params.out_max_indices`),
the layout pass MUST enforce a deterministic “fit” policy when a patch would exceed remaining
capacity. Two acceptable policies are:

- **Whole-draw overflow:** set `counters.overflow = 1` (or a layout-pass-local overflow flag) and
  force the indirect draw counts to 0 (draw nothing).
- **Per-patch clamping (recommended):** clamp `tess_level` down until the patch fits in the remaining
  space, and if it cannot fit even at `tess_level = 1`, drop the patch by writing
  `{tess_level=0, vertex_count=0, index_count=0}`.

The in-tree tessellation scaffolding uses a serial layout pass with per-patch clamping and writes
the indirect args directly; see `crates/aero-d3d11/src/runtime/tessellation/layout_pass.rs`.

#### 2.1.3) Geometry shader instancing (`SV_GSInstanceID`)

SM5 geometry shaders may declare an **instance count** (`dcl_gs_instance_count` / HLSL
`[instance(n)]`). This causes the GS to be invoked `n` times per input primitive, with
`SV_GSInstanceID` in `0..n`.

Let:

- `gs_instance_count` be the declared GS instance count (default 1 when the declaration is absent).

This affects:

- **GS dispatch sizing:** the GS compute pass must cover both dimensions (`primitive_id` and
  `gs_instance_id`).
- **Output sizing:** worst-case output bounds must be multiplied by `gs_instance_count` (see “GS
  output sizing” below).

#### 2.2) Scratch buffers (logical) and required WebGPU usages

The expansion pipeline uses per-draw (or per-encoder) scratch allocations. These are *logical*
buffers; they may be implemented as separate `wgpu::Buffer`s or as sub-allocations of a larger
transient arena, as long as alignment requirements are respected.

**Recommended allocation strategy: per-frame segmented scratch arena**

To avoid `createBuffer` churn and to ensure we never overwrite scratch that is still in-flight on
the GPU, the recommended implementation is a single large scratch buffer partitioned into
`frames_in_flight` segments (a ring):

- Backing buffer:
  - one `wgpu::Buffer` with a usage superset that covers all scratch slices:
    `STORAGE | VERTEX | INDEX | INDIRECT | COPY_SRC | COPY_DST`.
  - total size = `frames_in_flight * per_frame_capacity`.
- Per-frame arena:
  - each frame uses a disjoint `[base_offset, base_offset + per_frame_capacity)` region.
- allocations within a frame are bump-pointer suballocations aligned to:
    - `COPY_BUFFER_ALIGNMENT` (4),
    - `min_storage_buffer_offset_alignment` (usually 256) when binding as storage, and
    - `min_uniform_buffer_offset_alignment` (usually 256) when binding as uniform with dynamic
      offsets, and
    - 16 bytes for convenience (matches `vec4`-heavy structs).
- Lifetime:
  - call `begin_frame()` at a natural boundary where prior work is known to have been submitted,
    e.g. `PRESENT` or `FLUSH` in the command stream.
  - `begin_frame()` advances the ring index and resets the arena for that segment.

This is implemented in-tree as `ExpansionScratchAllocator` (`crates/aero-d3d11/src/runtime/expansion_scratch.rs`).
If you change the required scratch layouts/bindings, update the allocator usage accordingly.

**Alignment requirements:**

- Buffer sizes and offsets must be 4-byte aligned (`wgpu::COPY_BUFFER_ALIGNMENT`) because the
   pipeline uses `copy_buffer_to_buffer` and may clear/initialize buffers with writes.
- If sub-allocating a shared scratch buffer and binding with dynamic offsets, each slice must be
   aligned to `device.limits().min_storage_buffer_offset_alignment` (typically 256).

**Scratch allocations:**

1. **VS-out (`vs_out_regs`)**
    - Purpose: stores vertex shader output **registers** (control points) for the draw, consumable by
      HS/GS.
    - Usage: `STORAGE` (written/read by compute).
    - Layout: register buffer (`array<vec4<f32>>`), using a per-invocation stride in registers:
      - `vs_out_base = vs_out_index * VS_OUT_STRIDE`
      - output register `r` is stored at `vs_out_regs.data[vs_out_base + r]`
    - Total register count: `input_vertex_invocations * instance_count * VS_OUT_STRIDE`.
    - Note: `VS_OUT_STRIDE` is derived from the VS output signature (max output register index + 1),
      not from the final PS-linked varying count.

2. **HS-out control points (`hs_out_cp_regs`, optional; full HS/DS)**
    - Purpose: stores hull shader **output control-point** registers (per patch, per output control
      point). DS reads these as its “control point patch” input.
    - Usage: `STORAGE` (written/read by compute).
    - Layout: register buffer (`array<vec4<f32>>`), indexed compatibly with the in-tree HS→WGSL
      translator:
      - `hs_out_cp_base = (patch_instance_id * HS_MAX_CONTROL_POINTS + output_control_point_id) * HS_CP_OUT_STRIDE`
      - output register `r` is stored at `hs_out_cp_regs.data[hs_out_cp_base + r]`
    - Total register count (conservative): `patch_count * instance_count * HS_MAX_CONTROL_POINTS * HS_CP_OUT_STRIDE`.
      - Note: this uses `HS_MAX_CONTROL_POINTS = 32` even when the shader declares fewer output
        control points, to keep indexing stable and allow a fixed `x=0..32` dispatch shape.
    - Bring-up note (P2a): HS may be treated as pass-through and `hs_out_cp_regs` may alias
      `vs_out_regs`.
    - Binding (if using a stable internal layout):
      - baseline doc scheme: `@group(3) @binding(277)`
      - in-tree HS translator (per-pass internal group): `@group(0) @binding(1)`

3. **HS-out patch constants (`hs_out_pc_regs`, optional; full HS/DS)**
    - Purpose: stores hull shader **patch-constant** output registers (per patch). DS reads these as
      its “patch constant” input (in addition to tess factors).
    - Usage: `STORAGE` (written/read by compute).
    - Layout: register buffer (`array<vec4<f32>>`):
      - `hs_out_pc_base = patch_instance_id * HS_PC_OUT_STRIDE`
      - output register `r` is stored at `hs_out_pc_regs.data[hs_out_pc_base + r]`
    - Total register count: `patch_count * instance_count * HS_PC_OUT_STRIDE`.
    - Bring-up note (P2a): only tess factors are required, so implementations may omit this buffer
      and write tess factors directly to `tess_patch_constants` instead.
    - Binding (if using a stable internal layout):
      - baseline doc scheme: `@group(3) @binding(278)`
      - in-tree HS translator (per-pass internal group): `@group(0) @binding(2)`

4. **Tessellation-out (`tess_out_vertices`, `tess_out_indices`)**
    - Purpose: stores post-DS vertices + tessellator-generated indices (triangle list for tri/quad
      domains; required in the baseline P2 design).
    - Usage (vertices): `STORAGE | VERTEX`
    - Usage (indices): `STORAGE | INDEX`
    - Index element type (baseline): `u32` (`wgpu::IndexFormat::Uint32`). A future optimization may
      choose `u16` when the expanded vertex count is known to fit.
    - Capacity sizing: derived from tess factors. For P2a tri-domain and P2b quad-domain integer
      tessellation, see “Tessellation sizing” above (`V_total`, `I_total`).

5. **GS-out (`gs_out_vertices`, `gs_out_indices`)**
    - Purpose: stores post-GS vertices + indices suitable for final rasterization.
    - Usage (vertices): `STORAGE | VERTEX`
    - Usage (indices): `STORAGE | INDEX`
    - Index element type (baseline): `u32` (`wgpu::IndexFormat::Uint32`).
    - Capacity sizing: derived from `input_prim_count * instance_count * gs_maxvertexcount`, plus
      additional expansion when emitting list primitives without an index buffer (see below).

6. **Indirect args (`indirect_args`)**
    - Purpose: written by compute, consumed by render pass as indirect draw parameters.
    - Usage: `STORAGE | INDIRECT`

7. **Counters (`counters`)**
    - Purpose: atomic counters used during expansion (output vertex count, output index count,
      overflow flags).
    - Usage: `STORAGE` (atomics) and optionally `COPY_SRC` for debugging readback.

8. **Tessellation patch state (`tess_patch_state`)**
    - Purpose: per-patch metadata produced by HS patch-constant pass and consumed by tessellator/DS
      passes:
      - computed tessellation level(s),
      - allocated output ranges within `tess_out_vertices` / `tess_out_indices`.
    - Usage: `STORAGE` (read_write).
    - Layout: `array<TessPatchState>` (see below), 32 bytes per entry.
    - Entry count: `patch_count * instance_count`.

9. **Tessellation patch constants (`tess_patch_constants`)**
    - Purpose: per-patch tess factors (`SV_TessFactor` / `SV_InsideTessFactor`) produced by the HS
      patch-constant function and consumed by DS (and by the tessellator sizing policy).
    - Usage: `STORAGE` (read_write).
    - Layout: `array<TessPatchConstants>` (see below), 32 bytes per entry.
    - Entry count: `patch_count * instance_count`.

10. **GS input payload (`gs_inputs`, optional)**
    - Purpose: packed per-primitive GS inputs when using a GS translator that expects a dense
      register-file buffer (see “GS input register payload layout”).
    - Usage: `STORAGE` (declared as read_write for scratch-buffer compatibility; conceptually read-only).
    - Layout: `array<vec4<f32>>` registers packed by `((prim_id * verts_per_prim + vertex) * reg_count + reg)`.
    - Capacity sizing:
      - `input_prim_count * instance_count * GS_INPUT_VERTS_PER_PRIM * GS_INPUT_REG_COUNT` registers.

##### 2.2.1) HS stage interface buffers (full HS/DS; concrete register IO)

For full HS/DS execution (beyond the P2a pass-through HS bring-up), the HS needs explicit stage IO
buffers:

- control-point inputs (`hs_in`, typically sourced from `vs_out_regs`),
- control-point outputs (`hs_out_cp_regs`), and
- patch-constant outputs (`hs_out_pc_regs`).

The in-tree HS translator (`translate_hs` in `crates/aero-d3d11/src/shader_translate.rs`) models
these as **runtime-sized register buffers**:

```wgsl
struct HsRegBuffer { data: array<vec4<f32>>; }
```

**Indexing (matches in-tree translator; normative)**

Let:

- `patch_instance_id = instance_id * patch_count + patch_id`
- `HS_MAX_CONTROL_POINTS = 32` (D3D11 max; used for stable indexing even when outputcontrolpoints < 32)

Then:

- Control-point IO (inputs and outputs) uses:
  - `base = (patch_instance_id * HS_MAX_CONTROL_POINTS + output_control_point_id) * STRIDE`
  - register `r` is at `data[base + r]`
- Patch-constant outputs use:
  - `base = patch_instance_id * STRIDE`
  - register `r` is at `data[base + r]`

**Stride derivation**

Strides are **in registers** (not bytes):

- `HS_IN_STRIDE`: `max_used_input_v_reg + 1` (excluding HS system-value inputs)
- `HS_CP_OUT_STRIDE`: `max_output_control_point_o_reg + 1` (from OSGN)
- `HS_PC_OUT_STRIDE`: `max_patch_constant_o_reg + 1` (from PCSG/PSGN)

This keeps register addressing stable without having to precisely reproduce HLSL struct packing.

**Bind group layout strategies**

Two layouts are valid:

1. **Unified internal group (baseline doc design):** place these buffers in `@group(3)` at reserved
   `@binding >= 256`, alongside other internal expansion buffers and GS/HS/DS D3D resources.
2. **Per-pass internal group (matches in-tree HS/DS scaffolding):** bind stage IO buffers in a
   dedicated internal group (often `@group(0)` with small binding numbers), and keep HS/DS D3D
   resources in `@group(3)`. This requires the pipeline layout to include empty groups 1/2 so
   indices line up with the stage-scoped binding model.

**GS output sizing: strip → list**

Geometry shaders typically declare output topology as `line_strip` or `triangle_strip`. WebGPU can
render strips, but handling `CutVertex`/restart semantics is simplest if we **expand to lists** in
the compute pass:

- `line_strip` → line list
- `triangle_strip` → triangle list

This means the **final render pipeline topology** is derived from the *GS output declaration* (after
conversion), not from the input-assembler topology:

| GS declared output | Final render topology |
|---|---|
| `point` | `PointList` |
| `line_strip` | `LineList` |
| `triangle_strip` | `TriangleList` |

Implementations must ensure the pipeline cache / `PipelineKey` uses this final topology so the
passthrough draw uses the correct WebGPU `PrimitiveTopology`.

There are two valid implementation strategies:

1. **Non-indexed list emission (simplest):** write vertices into `gs_out_vertices` in list order
   (duplicating vertices as needed), and use `drawIndirect`.
2. **Indexed list emission (less duplication):** write one vertex per `EmitVertex` and generate a
   separate `gs_out_indices` list, then use `drawIndexedIndirect`.

Both strategies are valid. When implementing real GS bytecode execution, **indexed list emission**
(2) is generally preferable because it matches `EmitVertex` semantics naturally (one output vertex
per `emit`) and avoids duplicating vertex payloads; the in-tree GS→WGSL translator also uses this
approach for its initial supported subset (see `crates/aero-d3d11/src/runtime/gs_translate.rs`).

For sizing, define:

- `M = gs_maxvertexcount` from the GS bytecode.
- `max_list_vertices_per_input_prim` = worst-case number of **list vertices** emitted when expanding
  a strip into a list. (This is also the worst-case number of **list indices** when using strategy
  (2), since each list vertex corresponds to one index.)

Then the per-primitive bound is:

| GS declared output | Max `EmitVertex` per input prim | Max list vertices per input prim |
|---|---:|---:|
| `point` | `M` | `M` |
| `line_strip` | `M` | `2 * max(0, M - 1)` |
| `triangle_strip` | `M` | `3 * max(0, M - 2)` |

So a conservative capacity is:

```
out_max_vertices =
  input_prim_count * instance_count * gs_instance_count * max_list_vertices_per_input_prim
```

If using strategy (2), then:

- `out_max_vertices = input_prim_count * instance_count * gs_instance_count * M`
- `out_max_indices = input_prim_count * instance_count * gs_instance_count * max_list_vertices_per_input_prim`

**Strip → list emission algorithm (indexed; recommended)**

To implement `EmitVertex` / `CutVertex` for strip output topologies while ultimately rendering a list
topology, maintain per-invocation local strip assembly state:

- `emitted_count: u32` (clamped to `M`)
- `strip_len: u32` (length of the current strip since the last `CutVertex`)
- for `line_strip`: `prev: u32` (previous emitted vertex index)
- for `triangle_strip`: `prev0: u32`, `prev1: u32` (previous two emitted vertex indices)

On `CutVertex`:

- set `strip_len = 0`.

On each `EmitVertex`:

1. If `emitted_count >= M`, ignore the emit (D3D clamps by `maxvertexcount`).
2. Allocate one output vertex index `vtx_idx` and write the vertex payload.
3. Emit list indices based on the strip state:
   - `line_strip` → `line_list`:
     - if `strip_len >= 1`, append indices `(prev, vtx_idx)` (2 indices).
     - update `prev = vtx_idx`.
   - `triangle_strip` → `triangle_list`:
     - if `strip_len == 0`, set `prev0 = vtx_idx`.
     - else if `strip_len == 1`, set `prev1 = vtx_idx`.
     - else:
       - let `i = strip_len` (0-based length before appending this vertex).
       - to preserve strip winding, swap the first two vertices on odd `i`:
         - if `(i & 1) == 0`: emit `(prev0, prev1, vtx_idx)`
         - else:            emit `(prev1, prev0, vtx_idx)`
       - advance the strip window: `prev0 = prev1; prev1 = vtx_idx`.
4. Increment: `strip_len += 1`, `emitted_count += 1`.

When using a global append counter (`counters`) for output allocation, `vtx_idx` can be obtained via
`atomicAdd(&counters.out_vertex_count, 1)`, and list indices can be appended similarly. This matches
the approach used by the in-tree GS translator’s generated WGSL.

**Expanded vertex layout (target; bit-preserving):**

For compatibility with signature-driven stage linking *and* to preserve integer/float bit patterns,
expansion outputs store the same logical interface that the pixel shader consumes, but encoded as
raw 32-bit lanes:

- `pos_bits`: `vec4<u32>` containing IEEE-754 `f32` bits for `SV_Position`
- `varyings[i]`: `vec4<u32>` for the *i*th linked varying, where varyings are ordered by ascending
  `@location` number (i.e. `varying_locations` is a sorted list and `varyings[i]` corresponds to
  `@location(varying_locations[i])`).

One concrete layout:

```wgsl
// One entry per expanded vertex in the final *renderable* stream (typically post-DS or post-GS).
struct ExpandedVertex {
  pos_bits: vec4<u32>;
  varyings: array<vec4<u32>, VARYING_COUNT>;
}
```

Where `VARYING_COUNT` is the number of linked varyings in the VS/GS/DS → PS signature intersection.
A bit-preserving passthrough vertex shader would then:

- loads `pos_bits` and `bitcast<vec4<f32>>()` to write `@builtin(position)`, and
- for each varying location `N` (with dense index `i`), loads `varyings[i]` and bitcasts to
  `vec4<f32>`.
  - Implementation detail: the current SM4/SM5 WGSL translator intentionally normalizes all user
    varyings to `vec4<f32>` in both VS/GS/DS outputs and PS inputs to avoid cross-stage type
    mismatches when D3D signatures have different component masks (see `emit_vs_structs` /
    `emit_ps_structs` in `crates/aero-d3d11/src/shader_translate.rs`).

Stride (bytes):

```
expanded_vertex_stride = 16 * (1 + VARYING_COUNT)
```

This encoding is intentionally “register-like”: it avoids WGSL struct layout pitfalls and keeps the
expansion pipeline independent of scalar type.

**Vertex buffer view (for the final draw):**

The final expansion output buffer (`tess_out_vertices` or `gs_out_vertices`) is consumed by the
render pipeline either:

- by binding it as a WebGPU **vertex buffer**, or
- by binding it as a `var<storage, read>` buffer and doing vertex pulling in the passthrough VS.

The rest of this section describes the vertex-buffer strategy.

To match the packed `ExpandedVertex` layout above:

- Each `vec4<u32>` lane is represented as a `VertexFormat::Uint32x4` attribute.
- `pos_bits` is bound at a dedicated `@location(P)` chosen to not collide with PS varying locations.
- Each varying location `N` is bound at `@location(N)`.
- Attribute offsets are tightly packed 16-byte chunks:
  - `pos_bits` offset = 0
  - the `i`th varying in ascending-location order offset = `16 * (1 + i)`
- `array_stride = expanded_vertex_stride_bytes`

This keeps the final draw in the “normal” vertex-input path (no storage-buffer reads in the vertex
stage), while still being bit-preserving via `bitcast`.

Implementation note: the current in-tree executor uses **storage-buffer vertex pulling** from an
`ExpandedVertex` record (`pos: vec4<f32>` + `varyings: array<vec4<f32>, 32>`) at
`@group(3) @binding(BINDING_INTERNAL_EXPANDED_VERTICES)` (see `generate_passthrough_vs_wgsl` in
`crates/aero-d3d11/src/runtime/wgsl_link.rs`). When changing expanded-vertex layout conventions,
keep the compute prepass output and this passthrough VS generator in sync.

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

**Indirect args finalize step (required)**

At the end of expansion, a small compute dispatch (typically 1 workgroup) MUST write the final
indirect args based on the counters:

- Non-indexed:
  - `DrawIndirectArgs.vertex_count = counters.out_vertex_count`
- Indexed:
  - `DrawIndexedIndirectArgs.index_count = counters.out_index_count`

In both cases:

- If `counters.overflow != 0`, set the draw count(s) to 0 to deterministically skip the draw.
- `first_vertex/first_index/base_vertex = 0` because expansion output buffers are already in the
  correct coordinate space.
- `first_instance = 0`.
- `instance_count`:
  - either preserve the original D3D `instance_count`, or
  - flatten instancing and write `1` (P1 bring-up allowed).

#### 2.4) Pass sequence (per draw)

The following compute passes are inserted before the final render pass. Each pass is dispatched
with an implementation-defined workgroup size chosen by the translator/runtime.

1. **VS (compute variant): vertex pulling + VS execution**
    - Inputs:
      - IA vertex buffers + index buffer (internal bind group)
      - VS resources (still `@group(0)`; this is the existing stage-scoped VS bind group)
      - Draw parameters (first/vertex/index, base vertex, instance info)
    - Output: `vs_out_regs` register buffer for each input vertex invocation:
      - `vs_out_base = vs_out_index * VS_OUT_STRIDE`
      - `vs_out_regs.data[vs_out_base + r] = o_r` (for each VS output register `r`)
    - Dispatch mapping (recommended):
      - Use a 2D grid:
        - `global_invocation_id.x` = `vinv` in `0..input_vertex_invocations`
        - `global_invocation_id.y` = `instance_id` in `0..instance_count`
      - Write to:
        - `vs_out_index = instance_id * input_vertex_invocations + vinv`
      - System values:
        - non-indexed draws: `SV_VertexID = first_vertex + vinv`
        - indexed draws: `SV_VertexID` is resolved from the index buffer (apply `first_index` and
          `base_vertex`; see `IndexPullingParams`), and `vinv` corresponds to the “index-in-draw”
          (`0..index_count`).
        - `SV_InstanceID = first_instance + instance_id`

2. **Tessellation (optional): HS/DS emulation**
    - Trigger: `hs != 0 || ds != 0 || topology is patchlist`.
    - Hull shader structure note:
      - D3D11 hull shaders conceptually have two phases:
        - **control-point** (runs per output control point), and
        - **patch-constant** (runs once per patch to produce tess factors and patch constants).
      - In SM5 DXBC these are commonly encoded in a single instruction stream; a practical
        translation strategy is to split the module into two compute entry points using the first
        top-level `ret` as a phase boundary (this is what the in-tree HS translator does).
    - HS patch-constant pass (per patch):
      - Reads control points from `vs_out_regs`.
      - Let `patch_instance_id = instance_id * patch_count + patch_id` (see `tess_patch_state`
        indexing rule).
        - If using a flattened dispatch where `global_invocation_id.x` is `patch_instance_id`, derive:
          - `patch_id = patch_instance_id % patch_count`
          - `instance_id = patch_instance_id / patch_count`
      - Writes tess factors to `tess_patch_constants[patch_instance_id]` (`SV_TessFactor` /
        `SV_InsideTessFactor`) for consumption by the tessellation sizing/layout policy.
      - Full HS/DS note: when executing real HS patch-constant bytecode, the patch-constant function
        can also write *additional* user patch constants. A practical model (matching the in-tree HS
        translator) is to store the full patch-constant output register file in `hs_out_pc_regs`
        (one `vec4<f32>` per output register), and either:
        - have the tessellation layout pass read tess factors from those registers directly, or
        - additionally copy/pack tess factors into `tess_patch_constants` as the dedicated layout-pass
          input.
      - System values mapping (recommended):
        - `SV_PrimitiveID = patch_instance_id` (or `patch_id` if instancing is flattened).
      - Note: HS patch-constant produces the tess factors but does not inherently know output buffer
        capacities. Allocation of per-patch output ranges can be done either:
        - in a separate deterministic tessellation **layout pass** (recommended; see “Tessellation
          layout pass”), or
        - via atomics directly in the HS patch-constant pass (more parallel, but output ordering is
          implementation-defined).
      - Writes any additional patch constants needed by DS to scratch (per patch; P2 follow-up),
        typically via `hs_out_pc_regs`.
      - Dispatch mapping (recommended):
        - Use a flattened patch ID:
          - `global_invocation_id.x` = `patch_instance_id` (`0..(patch_count * instance_count)`)
    - Tessellation layout pass (recommended; deterministic):
      - Reads tess factors from `tess_patch_constants`.
      - Derives tessellation level(s) and stores them in `tess_patch_state[patch_instance_id]`:
        - P2a tri-domain: `tess_level_u = T`, `tess_level_v = 0`
        - P2b quad-domain: `tess_level_u = T_u`, `tess_level_v = T_v`
      - Computes per-patch counts (triangle list output) and base offsets:
        - tri domain: `vertex_count = (T + 1)(T + 2)/2`, `index_count = 3*T*T`
        - quad domain: `vertex_count = (T_u + 1)(T_v + 1)`, `index_count = 6*T_u*T_v`
        - Writes `{base_vertex, vertex_count, base_index, index_count}` into
          `tess_patch_state[patch_instance_id]` (prefix-sum base offsets).
      - Enforces capacity (`params.out_max_*`) deterministically (see “Tessellation layout pass”).
      - Writes the indirect args for the tess output stream when no GS is bound.
      - Dispatch mapping:
        - one workgroup, one active lane (`@workgroup_size(1)`; `global_invocation_id.x == 0`).
    - HS control-point pass (per patch control point):
      - Reads control points from `vs_out_regs`.
      - Writes HS output control points to scratch (`hs_out_cp_regs`). For P2a bring-up, HS may be a
        pass-through and `hs_out_cp_regs` may alias `vs_out_regs`.
      - System values mapping (recommended):
        - `SV_OutputControlPointID = control_point_id`
        - `SV_PrimitiveID = patch_instance_id`
      - Dispatch mapping:
        - Use a 2D grid (matches the in-tree HS translator’s shape):
          - `global_invocation_id.x` = `control_point_id` (`0..32`)
          - `global_invocation_id.y` = `patch_instance_id` (`0..(patch_count * instance_count)`)
        - Early-return if `control_point_id >= control_points`.
        - Derive `patch_id`/`instance_id` from `patch_instance_id` as described above when indexing
          per-instance `vs_out_regs` inputs.
    - Tessellator + DS evaluation:
      - Generates tessellated domain points and evaluates DS.
      - For P2a tri-domain, the doc specifies a concrete uniform grid enumeration and triangle-list
        index generation (see “Tri-domain grid enumeration” above).
      - For P2b quad-domain, see “Quad-domain grid enumeration” above.
      - Uses `tess_patch_state[patch_instance_id]` to coordinate per-patch output ranges
        (`base_vertex/base_index`) when emitting into the shared `tess_out_*` buffers.
      - DS consumes HS outputs:
        - control points from `hs_out_cp_regs` (or directly from `vs_out_regs` in pass-through HS
          mode), and
        - patch constants from `hs_out_pc_regs` (full HS/DS) and/or `tess_patch_constants`
          (tess-factor-only bring-up).
      - System values mapping (recommended):
        - `SV_DomainLocation`: computed from `domain_vertex_id` and the patch’s tess level(s) using
          the concrete enumeration rules above.
        - `SV_PrimitiveID = patch_instance_id`
      - Dispatch mapping (recommended; matches in-tree `runtime/tessellation/domain_eval.rs`):
        - Use a 2D grid where:
          - `global_invocation_id.x` = `patch_instance_id` (`0..(patch_count * instance_count)`)
          - `global_invocation_id.y` = `domain_vertex_id` (`0..V_patch_max`)
        - Choose a Y workgroup size (e.g. `DOMAIN_EVAL_WORKGROUP_SIZE_Y = 64`) and dispatch:
          - `dispatch_x = patch_count * instance_count`
          - `dispatch_y = ceil(V_patch_max / DOMAIN_EVAL_WORKGROUP_SIZE_Y)`
        - Early-return if `domain_vertex_id >= tess_patch_state[patch_instance_id].vertex_count` (or
          if `tess_level_u == 0`).
        - Derive `patch_id`/`instance_id` from `patch_instance_id` when indexing per-instance inputs.
      - Writes `tess_out_vertices` + `tess_out_indices` (index generation may be a separate pass).
      - Indirect args (when no GS is bound):
        - if using the deterministic tessellation layout pass, it typically writes `indirect_args`
          directly and no additional finalize step is required, or
        - if using atomic/counter-based allocation, write final `indirect_args` via the standard
          finalize step.
      - If a **GS** is bound, treat `tess_out_*` as an intermediate stream and proceed to the GS
        phase; the final `indirect_args` must be written for the GS output stream instead.

3. **GS (optional): geometry shader emulation**
    - Trigger: `gs != 0` or adjacency topology.
      - Reads primitive inputs from the previous stage output (`vs_out_regs` for no tessellation,
        otherwise `tess_out_vertices` + `tess_out_indices`).
      - Optionally, implementations may pack upstream stage output registers into a dense `gs_inputs`
        buffer (see “GS input register payload layout”) and have the translated GS read from that.
      - Note (current in-tree translated-GS prepass):
        - Point-list, line-list, triangle-list, and adjacency-list (`LINELIST_ADJ`/`TRIANGLELIST_ADJ`)
          draws populate `gs_inputs` from **VS outputs** via a separate
          VS-as-compute input-fill pass (vertex pulling + a minimal SM4 VS subset). If VS-as-compute
          translation fails, the executor only falls back to IA-fill when the VS is a strict
          passthrough (or `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is set to force IA-fill for
          debugging; may misrender). Otherwise the draw fails with a clear error.
      - If the draw ran a tessellation phase, the runtime should reset `counters` + `indirect_args`
        before starting GS allocation (since GS writes a new output stream into `gs_out_*`).
      - Dispatch shape / ordering:
        - **Current in-tree strategy (`runtime/gs_translate.rs`):**
        - Dispatch the translated GS compute entry point (`gs_main`) with `@workgroup_size(1)` and
          `dispatch_workgroups(input_prim_count, 1, 1)`.
        - Each invocation processes one input primitive (`prim_id = global_invocation_id.x`) and
          loops `gs_instance_id` in `0..gs_instance_count` (matching `SV_GSInstanceID` semantics).
        - Output allocation uses atomic counters; strip→list conversion and `cut`/restart semantics
          are handled in WGSL.
        - After `gs_main`, dispatch a 1-workgroup finalize entry point (`cs_finalize`) to write
          `DrawIndexedIndirectArgs` from the counters and to deterministically suppress the draw on
          overflow.
      - **Synthetic-expansion prepass (scaffolding):** the built-in prepass WGSL in the executor
        (`GEOMETRY_PREPASS_CS_WGSL`) uses `dispatch_workgroups(primitive_count, gs_instance_count, 1)`
        and treats `global_invocation_id.y` as `SV_GSInstanceID`.
    - Emits vertices/indices to `gs_out_*`, updates counters, then finalizes indirect args.

4. **Final render**
    - Uses a render pipeline consisting of:
      - A **passthrough vertex shader** that reads `ExpandedVertex` from the final expansion output
        buffer and outputs the same `@location`s expected by the pixel shader.
      - The original pixel shader.
    - Issues `drawIndirect` or `drawIndexedIndirect` depending on whether an index buffer was
      generated.
      - When drawing indexed expansion output, bind the generated index buffer as
        `wgpu::IndexFormat::Uint32` (baseline).

**Passthrough VS strategy (concrete)**

The final render stage uses a small **passthrough vertex shader** plus the original pixel shader.
The current in-tree executor generates the passthrough VS WGSL on demand (see
`generate_passthrough_vs_wgsl` in `crates/aero-d3d11/src/runtime/wgsl_link.rs`). The shader performs
**storage-buffer vertex pulling** from an `ExpandedVertex` record bound at
`@group(3) @binding(BINDING_INTERNAL_EXPANDED_VERTICES)`. Conceptually:

```wgsl
struct ExpandedVertex {
  pos: vec4<f32>,
  varyings: array<vec4<f32>, 32>,
};

@group(3) @binding(BINDING_INTERNAL_EXPANDED_VERTICES)
var<storage, read> expanded_vertices: array<ExpandedVertex>;

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(1) o1: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
  let v = expanded_vertices[vertex_index];
  var out: VsOut;
  out.pos = v.pos;
  out.o1 = v.varyings[1u];
  return out;
}
```

Notes:

- The executor links the expanded VS and the application PS by trimming unused PS inputs / VS
  outputs. If the PS reads a varying location the passthrough VS cannot provide, the draw fails with
  a clear error.
- The baseline expanded vertex record stores `@location(0..31)` varyings
  (`EXPANDED_VERTEX_MAX_VARYINGS`, currently 32). If a pixel shader reads a higher `@location`, the
  executor fails pipeline creation with a clear error.
- Implementation note (in-tree executor): the render pipeline layout must include the reserved
  internal-emulation bind group (`@group(3)`) so the generated passthrough VS can read the expanded
  vertex buffer. The executor extends the pipeline layout accordingly (see
  `extend_pipeline_bindings_for_passthrough_vs` in
  `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).

#### 2.5) Render-pass splitting constraints

WebGPU forbids running compute work inside an active render pass. The D3D11 command stream has no
explicit “pass” boundaries, so the executor must be prepared to split:

- If a render pass is currently open (batched draws with the same attachments) and we encounter a
  draw that triggers compute expansion, we MUST:
  1. end the current render pass (`StoreOp::Store`),
  2. run the compute expansion passes,
  3. begin a new render pass targeting the same attachments with `LoadOp::Load`, and
  4. re-apply dynamic state (viewport/scissor/stencil ref) before continuing.

This preserves D3D semantics but increases render pass count and can affect performance (expected).

### 3) Binding model for emulation kernels

#### 3.1) User (D3D) resources for GS/HS/DS

GS/HS/DS are compiled as compute entry points but keep the normal D3D binding model:

- D3D resources live in `@group(3)` (the reserved GS/HS/DS + internal emulation group) and use the
  same binding number scheme as other stages:
  - `b#` (cbuffers) → `@binding(BINDING_BASE_CBUFFER + slot)`
  - `t#` (SRVs)     → `@binding(BINDING_BASE_TEXTURE + slot)`
  - `s#` (samplers) → `@binding(BINDING_BASE_SAMPLER + slot)`
  - `u#` (UAVs, SM5) → `@binding(BINDING_BASE_UAV + slot)` (where supported)
- Resource-binding opcodes select whether a binding targets CS (`@group(2)`) vs GS/HS/DS (`@group(3)`)
  either via the direct `shader_stage = GEOMETRY` encoding (preferred for GS), or via the `stage_ex`
  encoding (`shader_stage = COMPUTE`, non-zero `reserved0`) required for HS/DS (and optionally used
  for GS).

#### 3.2) Internal bind groups and reserved bindings

Expansion compute pipelines require additional buffers that are not part of the D3D binding model
(vertex pulling inputs, scratch outputs, counters, indirect args).

Implementation note: the in-tree executor uses a **mixed** approach:

- Most compute-prepass IO (expanded vertices/indices, counters/indirect args, uniforms like
  `{primitive_count, instance_count}`) is bound in a **per-pass internal bind group** (typically
  `@group(0)` with small binding numbers). This keeps prepass IO decoupled from the D3D binding model.
- `@group(3)` is reserved for extended D3D stages (GS/HS/DS) plus any internal helpers that must
  coexist with those stage bindings. Any such internal helpers use `@binding >= BINDING_BASE_INTERNAL
  = 256` so they stay disjoint from the D3D register-space ranges (`b#`/`t#`/`s#`/`u#`).
  - This split is also used to work within downlevel backend constraints (e.g.
    `max_storage_buffers_per_shader_stage` can be as low as 4): the in-tree GS prepass uses separate
    compute pipelines for GS-input fill (IA-fill or VS-as-compute, depending on topology) vs GS
    execution so vertex pulling storage buffers are not bound alongside multiple expansion output
    storage buffers.

##### 3.2.1) Current in-tree layout: per-pass internal `@group(0)` + stage-scoped `@group(3)`

The current in-tree emulation kernels generally use a **dedicated internal bind group**
(`@group(0)` with small binding numbers) for stage IO / scratch buffers, and keep GS/HS/DS **D3D
resources** in `@group(3)`:

- Synthetic-expansion prepass (`GEOMETRY_PREPASS_CS_WGSL` in the executor): expansion outputs +
  params in `@group(0)`, GS `cb#[]` in `@group(3)`.
  - Translated GS DXBC prepass (`runtime/gs_translate.rs`): expansion outputs + counters/indirect args +
  params + `gs_inputs` in `@group(0)`, and referenced GS resources (`cb#`/`t#`/`s#`) in `@group(3)`.
  - The in-tree translated-GS prepass builds `gs_inputs` via a separate **input fill** compute pass:
    - IA vertex pulling bindings live in `@group(3)` (internal range), and the input-fill kernel
      writes the packed register payload into a `@group(0)` storage buffer (`gs_inputs`).
    - Point-list, line-list, triangle-list, and adjacency-list (`LINELIST_ADJ`/`TRIANGLELIST_ADJ`)
      draws prefer VS-as-compute for this payload (minimal SM4 VS subset)
      so the GS observes VS output registers. If VS-as-compute translation fails, the executor only
      falls back to IA-fill when the VS is a strict passthrough (or
      `AERO_D3D11_ALLOW_INCORRECT_GS_INPUTS=1` is set to force IA-fill for debugging; may misrender).
      Otherwise the draw fails with a clear error.
- HS translation (`translate_hs` in `shader_translate.rs`): `hs_in/hs_out_cp/hs_out_pc` at
  `@group(0) @binding(0..2)`, HS resources in `@group(3)`.
- Tessellation DS evaluation (`runtime/tessellation/domain_eval.rs`): patch meta + HS outputs at
  `@group(0)`, DS resources in `@group(3)`.

This works because WebGPU bind groups are **per pipeline**: internal compute pipelines can choose
different group layouts than application render pipelines. When using this layout style, the
pipeline layout should still include empty group layouts for groups 1 and 2 so indices line up with
the stage-scoped binding model (VS/PS/CS).

These `@group(0)` bindings are not part of the D3D binding model, so they can use low binding numbers
without colliding with stage-scoped D3D resource bindings.

Internal-only helpers that *do* live in `@group(3)` (because they must coexist with GS/HS/DS resource
tables) use `@binding >= BINDING_BASE_INTERNAL = 256`. Today this includes IA vertex pulling
(`runtime/vertex_pulling.rs`) and index pulling (`runtime/index_pulling.rs`).

Let:

- D3D resource bindings occupy:
  - `@binding(0..BINDING_BASE_TEXTURE)` for cbuffers
  - `@binding(BINDING_BASE_TEXTURE..BINDING_BASE_SAMPLER)` for SRVs
  - `@binding(BINDING_BASE_SAMPLER..BINDING_BASE_UAV)` for samplers
  - `@binding(BINDING_BASE_UAV..BINDING_BASE_UAV + MAX_UAV_SLOTS)` for UAVs
- Expansion-internal bindings start at `BINDING_BASE_INTERNAL = 256`.

Target unified binding assignments (design notes; only partially implemented today):

- `@binding(256)`: `ExpandParams` (uniform/storage; IA offsets/strides + draw/topology info)
- `@binding(257..=264)`: vertex buffers `vb0..vb7` as read-only storage (after slot compaction)
- `@binding(265)`: `IndexPullingParams` (uniform; indexed draws only; optional helper binding)
- `@binding(266)`: index buffer as read-only storage (`array<u32>` words; indexed draws only; absent → dummy)
- `@binding(267)`: `vs_out_regs` (read_write storage; VS stage IO register buffer)
- `@binding(268)`: `tess_out_vertices` (read_write storage)
- `@binding(269)`: `tess_out_indices` (read_write storage)
- `@binding(270)`: `gs_out_vertices` (read_write storage)
- `@binding(271)`: `gs_out_indices` (read_write storage)
- `@binding(272)`: `indirect_args` (read_write storage)
- `@binding(273)`: `counters` (read_write storage; atomics)
- `@binding(274)`: `tess_patch_state` (read_write storage; per patch, used by HS/DS emulation)
- `@binding(275)`: `tess_patch_constants` (read_write storage; per patch tess factors for DS)
- `@binding(276)`: `gs_inputs` (read_write storage; conceptually read-only; optional packed GS input
  register payload)
- `@binding(277)`: `hs_out_cp_regs` (read_write storage; optional HS output control-point registers)
- `@binding(278)`: `hs_out_pc_regs` (read_write storage; optional HS patch-constant output registers)

Additional reserved internal bindings (in-tree; render-stage support)

The in-tree binding model (`crates/aero-d3d11/src/binding_model.rs`) also reserves a small block of
internal bindings for the *render-stage* “expanded draw” path:

- `@binding(384)`: expanded vertices (`BINDING_INTERNAL_EXPANDED_VERTICES`)
- `@binding(385)`: expanded indices (`BINDING_INTERNAL_EXPANDED_INDICES`)
- `@binding(386)`: draw params (`BINDING_INTERNAL_DRAW_PARAMS`)

These are disjoint from the `256..=278` range above and may be bound by the executor even when a
given passthrough VS consumes the expanded vertex buffer via normal vertex inputs (see “Passthrough
VS strategy” notes above).

**`ExpandParams` layout (concrete; `@binding(256)`)**

The expansion pipeline needs a small, stable parameter block that covers:

- the original D3D draw parameters (`Draw` / `DrawIndexed`),
- IA topology,
- IA buffer offsets/strides (because D3D offsets are not guaranteed to be 256-byte aligned and
  therefore cannot be expressed purely via WebGPU buffer-binding offsets), and
- output capacities (for overflow protection).

Recommended packed layout (little-endian, 4-byte fields):

```c
// Total size: 192 bytes (aligned to 16 for WGSL uniform layout compatibility).
//
// Note: `var<uniform>` layout rules effectively require 16-byte struct alignment and 16-byte array
// strides. The explicit `_pad*` fields below are required so the C/host-side byte layout matches the
// WGSL layout deterministically (same approach as `AeroVpIaSlot` in `vertex_pulling.rs`).
struct AerogpuExpandVertexBuffer {
  uint32_t base_offset_bytes; // IA VB binding offset in bytes
  uint32_t stride_bytes;      // IA VB binding stride in bytes
  uint32_t _pad0;
  uint32_t _pad1;
};

// Total size: 192 bytes.
struct AerogpuExpandParams {
  // Compact IA vertex-buffer bindings (after slot compaction).
  // Each entry corresponds to vbN in `@binding(257 + N)`.
  struct AerogpuExpandVertexBuffer vb[8];

  // Vertex pulling draw parameters (matches the tail of `AeroVpIaUniform` when using a fixed 8-slot
  // IA uniform).
  uint32_t first_vertex;
  uint32_t first_instance;
  int32_t  base_vertex; // 0 for Draw
  uint32_t first_index; // 0 for Draw

  // Draw + topology.
  uint32_t draw_kind;   // 0 = Draw, 1 = DrawIndexed
  uint32_t topology;    // enum aerogpu_primitive_topology (including adj/patchlist extensions)
  uint32_t vertex_count;
  uint32_t index_count; // 0 for Draw

  // Draw metadata.
  uint32_t instance_count;
  uint32_t index_format;      // enum aerogpu_index_format (valid only for DrawIndexed)
  uint32_t index_offset_bytes; // IA index-buffer binding offset in bytes (optional; see note below)
  uint32_t expanded_vertex_stride_bytes;

  // Output capacities (elements, not bytes).
  uint32_t out_max_vertices;
  uint32_t out_max_indices; // 0 if unused
  uint32_t _pad0;
  uint32_t _pad1;
};
```

WGSL-side, this is typically declared as:

```wgsl
struct ExpandParams { /* same fields */ }
@group(3) @binding(256) var<uniform> params: ExpandParams;
```

**Vertex pulling compatibility note (internal layout)**

The `vb[8]` array is intentionally a fixed-size array that matches WebGPU’s baseline vertex-buffer
limit (`MAX_WGPU_VERTEX_BUFFERS = 8`). This makes the `ExpandParams` uniform layout stable across
draws, and ensures the draw-parameter tail (`first_vertex/first_instance/base_vertex/first_index`)
is always at a fixed byte offset.

For draws that use fewer than 8 vertex buffers after slot compaction, the runtime MUST:

- Set unused `vb[i].{base_offset_bytes,stride_bytes} = 0`, and
- Bind the corresponding `vb{i}` storage bindings (`@binding(257 + i)`) to a small dummy buffer so
  the bind group layout remains stable.

Note: D3D11 index-buffer binding offsets are not guaranteed to be 256-byte aligned, so for compute
index pulling it is often simplest to fold the IA index-buffer byte offset into `first_index` on
the host (if the offset is stride-aligned). In that case, set `index_offset_bytes = 0` and treat
`first_index` as the fully-adjusted first index. This is what the current in-tree executor does for
its compute prepass.

**`IndexPullingParams` layout (concrete; `@binding(265)`; optional helper)**

The in-tree index-pulling WGSL helper (`crates/aero-d3d11/src/runtime/index_pulling.rs`) models the
index buffer as `array<u32>` and uses a tiny uniform for `{first_index, base_vertex, index_format}`.
This binding is optional if your expansion shaders read those fields from `ExpandParams` directly,
but reserving it makes it easy to reuse the helper as-is:

```wgsl
struct IndexPullingParams {
  first_index: u32;
  base_vertex: i32;
  index_format: u32; // 0 = u16, 1 = u32
  _pad0: u32;
}
@group(3) @binding(265) var<uniform> ip: IndexPullingParams;
@group(3) @binding(266) var<storage, read> index_words: array<u32>;
```

For non-indexed draws, implementations that use a stable internal bind group layout SHOULD still
bind a zeroed `IndexPullingParams` and a dummy `index_words` buffer; the compute kernels will not
read them when `draw_kind == Draw`.

**`counters` layout (concrete; `@binding(273)`)**

The counters buffer is written by expansion passes and read when finalizing indirect args:

```wgsl
struct ExpandCounters {
  out_vertex_count: atomic<u32>;
  out_index_count: atomic<u32>;
  overflow: atomic<u32>; // 0/1 (set when any pass exceeds out_*_max)
  _pad0: u32;
}
@group(3) @binding(273) var<storage, read_write> counters: ExpandCounters;
```

**Counter usage (allocation + overflow; normative)**

Expansion passes allocate output space by atomically incrementing counters:

- To append `n` vertices:
  - `base = atomicAdd(&counters.out_vertex_count, n)`
  - If `base + n > params.out_max_vertices`, set `atomicStore(&counters.overflow, 1)` and **do not
    write** (to avoid out-of-bounds writes).
- To append `n` indices:
  - `base = atomicAdd(&counters.out_index_count, n)`
  - If `base + n > params.out_max_indices`, set overflow and do not write.

Note: `atomicAdd` will still increment the counter even in the overflow case. This is fine because
the finalize step MUST turn the indirect draw count(s) into 0 when `overflow != 0`, so the render
pass deterministically draws nothing.

**Multi-phase expansion note (tess → GS)**

The `counters` buffer represents the allocation state for a *single* output stream at a time (the
stream whose `{out_*_count, overflow}` will be consumed by the subsequent indirect-draw finalize
step). If a draw runs multiple expansion phases that each allocate into different output buffers
(e.g. tessellation allocates `tess_out_*` and then a GS allocates `gs_out_*`), the runtime MUST:

- reset `counters` (and clear the relevant `indirect_args` struct) before starting the next
  allocation phase, and
- run the indirect-args finalize step only for the **final** output stream that will be rendered.

Implementations may choose to simplify this by rejecting “HS/DS + GS” combined pipelines for initial
bring-up (as described in the P2 limitations section), but the general pipeline shape should follow
the rules above when supported.

**`tess_patch_state` layout (concrete; `@binding(274)`)**

Tessellation requires per-patch state that is produced by the HS patch-constant pass and then
consumed by subsequent tessellator/DS passes (and optionally GS).

Recommended layout (32 bytes per patch):

```wgsl
struct TessPatchState {
  // Tessellation level for the patch after clamping/rounding.
  //
  // For P2a tri-domain integer tessellation, `tess_level_u` stores `T` and `tess_level_v` is 0.
  // For future quad-domain tessellation, `tess_level_u/v` can store independent U/V levels.
  //
  // `tess_level_u == 0` indicates the patch was dropped (e.g. due to scratch capacity limits) and
  // subsequent passes must treat `vertex_count/index_count` as 0.
  tess_level_u: u32;
  tess_level_v: u32;

  // Allocated output ranges (in element counts, not bytes).
  base_vertex: u32;
  vertex_count: u32;
  base_index: u32;
  index_count: u32;

  _pad0: u32;
  _pad1: u32;
}
// Bind group index is `3` in the baseline design (shared with GS/HS/DS resources and other internal
// emulation bindings).
@group(3) @binding(274) var<storage, read_write> tess_patch_state: array<TessPatchState>;
```

Entry count: `patch_count * instance_count` (one patch-state entry per patch per instance).

Indexing rule (normative):

- `patch_instance_id = instance_id * patch_count + patch_id`
- `tess_patch_state[patch_instance_id]` corresponds to `(patch_id, instance_id)`.

Implementation note: the in-tree tessellation scaffolding currently uses a compact 20-byte per-patch
metadata struct (`TriDomainPatchMeta` / `TessellationLayoutPatchMeta` in
`crates/aero-d3d11/src/runtime/tessellation/*`) that stores:
`{tess_level, vertex_base, index_base, vertex_count, index_count}` (5×`u32`). This doc’s 32-byte
`TessPatchState` is a superset that reserves space for future quad-domain `(u, v)` levels. Either
layout is valid as long as all passes agree on the same struct.

**`tess_patch_constants` layout (concrete; `@binding(275)`)**

The HS patch-constant function produces tessellation factors and (optionally) other patch-constant
data. DS consumes the patch-constant data, including the tess factors.

For P2 bring-up we start by supporting the tess factors themselves (no additional user patch
constants yet).

Recommended layout (32 bytes per patch):

```wgsl
struct TessPatchConstants {
  // Edge tess factors (f32 values) packed into a 4-wide vector for simplicity.
  //
  // Domain mapping:
  // - tri domain:   edge[0..2] = xyz, edge[3] unused
  // - quad domain:  edge[0..3] = xyzw
  // - isoline:      edge[0..1] = xy,  edge[2..3] unused
  edge_factors: vec4<f32>;

  // Inside tess factors (f32 values).
  //
  // Domain mapping:
  // - tri domain:   inside[0] = x, inside[1..3] unused
  // - quad domain:  inside[0..1] = xy, inside[2..3] unused
  // - isoline:      unused (all zero)
  inside_factors: vec4<f32>;
}
// Bind group index is `3` in the baseline design (shared with GS/HS/DS resources).
@group(3) @binding(275) var<storage, read_write> tess_patch_constants: array<TessPatchConstants>;
```

HS writes `f32` tess factors directly. Unused lanes MUST be written as `0.0` so tools and debug
readbacks are deterministic.

Implementation note: the in-tree tessellation layout pass (`runtime/tessellation/layout_pass.rs`)
currently models tri-domain tess factors using a compact `array<vec4<f32>>` layout where the four
lanes are `(edge0, edge1, edge2, inside)` (16 bytes per patch). This doc’s 32-byte layout extends
that scheme to cover quad-domain factors while keeping P2a tri-domain values compatible (unused
lanes are zero).

Indexing rule (same as `tess_patch_state`):

- `patch_instance_id = instance_id * patch_count + patch_id`
- `tess_patch_constants[patch_instance_id]` corresponds to `(patch_id, instance_id)`.

**Initialization requirements**

Before running expansion for a draw (and before starting any additional allocation phase within the
same draw, e.g. tessellation followed by GS), the runtime MUST initialize the relevant scratch
state:

- Zero `counters` (all fields, including atomics).
- Zero the relevant `indirect_args` struct (so a failed/overflowed expansion deterministically draws
  nothing).

This can be done with `CommandEncoder::clear_buffer` / `wgpu::CommandEncoder::clear_buffer` when
available, or via a small `queue.write_buffer`/copy-from-zero-buffer fallback.

Note: vertex pulling requires reading the guest’s bound vertex/index buffers from compute. The
host must therefore create buffers with `AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER` /
`AEROGPU_RESOURCE_USAGE_INDEX_BUFFER` using WebGPU usages that include `STORAGE` (in addition to
`VERTEX`/`INDEX`), so they can be bound as read-only storage buffers for vertex pulling.

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

For GS opcode discovery / fixture authoring, use the in-repo DXBC token dump tool:

```bash
# Compile an SM4 geometry shader (example using fxc from the Windows SDK).
fxc /T gs_4_0 /E main /Fo shader.dxbc shader.hlsl

# Dump the DXBC container + SM4 token stream (opcode IDs, lengths, best-effort decode).
cargo run -p aero-d3d11 --bin dxbc_dump -- shader.dxbc
```

**Pixel-compare tests (Rust):**

Add new `aero-d3d11` executor tests that render to an offscreen RT and compare readback pixels:

- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_*.rs`
  - Example (strip cut/restart semantics): `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs`
  - Example (DrawIndexed GS prepass): `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_pointlist_draw_indexed.rs`
  - Example (adjacency-list GS inputs): `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_linelistadj_emits_triangle.rs`,
    `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelistadj_emits_triangle.rs`
  - Example (compute-prepass plumbing smoke): `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs`
- Primitive restart (indexed strip topologies):
  - `crates/aero-d3d11/tests/aerogpu_cmd_primitive_restart.rs` (render coverage)
  - `crates/aero-d3d11/tests/aerogpu_cmd_primitive_restart_primitive_id.rs` (`SV_PrimitiveID` preservation)
  - `crates/aero-d3d11/tests/d3d11_runtime_strip_restart.rs` (`D3D11Runtime` coverage)
- `crates/aero-d3d11/tests/aerogpu_cmd_tessellation_*.rs`

Each test should:

1. Upload VS/PS (+ GS/HS/DS) fixtures.
2. Bind topology (including adjacency/patchlist where relevant).
3. Issue a draw that exercises the expansion path.
4. Read back the render target and compare to a tiny reference image (or a simple expected pattern).

Now that a minimal point-list, line-list, triangle-list, and adjacency-list (`LINELIST_ADJ`/`TRIANGLELIST_ADJ`)
translated GS DXBC execution path exists, keep the existing
“ignore GS payloads” robustness test
(`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_ignore.rs`) using a GS DXBC payload that is
intentionally **outside** the translator/execution subset. This test is meant to be a cheap
regression check that the executor:

- accepts and stores unsupported GS shaders (rather than failing with a stage-mismatch error), and
- accepts geometry-stage `stage_ex` bindings / extended `BIND_SHADERS` packets without crashing.

As guest GS DXBC execution expands, update the test to continue exercising the intended
“unsupported GS must not crash” forward-compat behavior (by keeping the fixture unsupported).

**Unit tests (non-rendering):**

Pixel-compare scenes catch many issues, but the emulation path also needs cheap unit tests that can
run in CI without a GPU backend:

1. **Protocol decoding/encoding**
   - `stage_ex` encode/decode rules (legacy vs extended paths)
   - `BIND_SHADERS` size handling (24 vs 36 bytes)
   - topology decoding for adjacency + patchlists
   - Location: `emulator/protocol/tests/*` (Rust + TS mirrors).

2. **Sizing functions**
   - topology → input primitive count calculations (strip/list/adj/patch)
   - conservative output buffer sizing for strip→list expansion
   - Location: pure Rust tests under `crates/aero-d3d11/tests/*`.

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

7. **GS fast-path (point sprite)**: input `pointlist` + GS expands to quads; verify UVs and
   per-sprite size from a cbuffer.
8. **GS compute-path (variable emit)**: GS emits 0..N triangles depending on a uniform; validates
   indirect-draw arg generation and counter reset.
9. **GS compute-path (RestartStrip)**: GS outputs a triangle strip with multiple `CutVertex` /
   `RestartStrip()` boundaries; validates strip→list conversion.
10. **GS instancing (`SV_GSInstanceID`)**: GS compiled with `[instance(2)]` uses `SV_GSInstanceID` to
    offset/colour output; validates the GS instance dispatch dimension and built-in mapping.
11. **Adjacency input**: adjacency-list topology (`LINELIST_ADJ` / `TRIANGLELIST_ADJ`) + GS consumes
    adjacency vertices; validates adjacency primitive assembly + translated GS execution (see
    `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_{linelistadj,trianglelistadj}_emits_triangle.rs`).

### P2 scenes

12. **Compute blur**: run CS to blur a texture then render it
13. **UAV write**: CS writes to structured buffer; PS reads and visualizes
14. **Tess P2a (tri domain, integer)**: `PATCHLIST_3` + simple HS/DS produces a subdivided triangle
    grid (color = `SV_DomainLocation`), validates patchlist topology + HS/DS execution.
15. **Tess P2b (quad domain, integer)**: `PATCHLIST_4` + simple HS/DS produces a subdivided quad.

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
