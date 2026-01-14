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
> - GS/HS/DS support via `*_stage_ex` shader-stage extensions
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
- `@group(3)`: geometry/hull/domain (“stage_ex”) resources (executed via compute emulation)
- `@group(4)`: internal emulation pipelines (`BIND_GROUP_INTERNAL_EMULATION` in
  `crates/aero-d3d11/src/binding_model.rs`)
  - This requires a device limit `maxBindGroups >= 5`. The baseline WebGPU design can instead merge
    internal-only bindings into `@group(3)` using the reserved `@binding >= 256` range.

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
- `@group(3)`: geometry/hull/domain (“stage_ex”) resources (tracked separately from CS to avoid clobbering)
- `@group(4)`: internal emulation pipelines (vertex pulling, expansion scratch, counters, indirect args)

Why stage-scoped?

- D3D11 resource bindings are tracked per-stage, and stages can be rebound independently.
- Using stage-scoped bind groups lets the runtime keep simple shadow-state and caches per stage:
  rebinding VS resources only invalidates/rebuilds `group(0)`, PS only touches `group(1)`, etc.
- It also keeps pipeline layout assembly straightforward:
  - Render pipelines use the VS + PS group layouts (0 and 1).
  - Compute pipelines use the CS layout (2).
  - Emulation pipelines (GS/HS/DS expansion) may additionally use:
    - `@group(3)` for GS/HS/DS D3D resources, and
    - `@group(4)` for internal emulation buffers (vertex pulling, scratch, counters, indirect args).

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
- The AeroGPU command stream distinguishes “which D3D stage is being bound” using the `stage_ex`
  extension carried in reserved fields of the resource-binding opcodes (see the ABI section below).
- Expansion-specific internal buffers (vertex pulling inputs, scratch outputs, counters, indirect
  args) are internal to the compute-expansion pipeline. In the baseline design these live alongside
  GS/HS/DS resources in the reserved extended-stage bind group (`@group(3)`) using a reserved
  binding-number range starting at `BINDING_BASE_INTERNAL = 256` (see “Internal bindings” below).
  - Implementations may temporarily place these in a dedicated internal bind group (`@group(4)`,
    `BIND_GROUP_INTERNAL_EMULATION`) as long as the device supports at least 5 bind groups.
  - Note: the current executor’s placeholder compute-prepass still uses an ad-hoc bind group layout
    for some output buffers, but vertex pulling already uses the reserved internal binding range.
  - Implementation detail: the in-tree vertex pulling WGSL uses `@group(3)` (see
    `VERTEX_PULLING_GROUP` in `crates/aero-d3d11/src/runtime/vertex_pulling.rs`) and pads pipeline
    layouts with empty groups so indices line up. Because the binding numbers are already in the
    reserved `>= 256` range, it can safely coexist with `stage_ex` GS/HS/DS bindings in the same
    group without collisions.
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
- **Typed UAV textures** (`RWTexture*` / `u#` storage textures) and the required format plumbing.
- **Atomics** (`Interlocked*`) and the necessary WGSL atomic type mapping.
- **Explicit ordering/barriers** (D3D UAV barriers, `GroupMemoryBarrier*` / `DeviceMemoryBarrier*` semantics).

---

## Geometry + Tessellation Emulation (GS/HS/DS) via Compute Expansion (P1/P2)

WebGPU exposes only **vertex** + **fragment** (render) and **compute** pipelines. D3D10/11’s
**GS/HS/DS** stages are therefore implemented by an explicit **compute-expansion pipeline** that:

> Quick overview + current limitations: see
> [`docs/graphics/geometry-shader-emulation.md`](./graphics/geometry-shader-emulation.md).

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
    - no stream-out / transform feedback (`CreateGeometryShaderWithStreamOutput`),
    - only stream 0 (no `EmitStream` / `CutStream` / `SV_StreamID`),
    - adjacency input primitives (`*_ADJ` topologies / `lineadj`/`triadj`) are initially unsupported;
      the runtime must reject them deterministically until the adjacency path is implemented,
    - **primitive restart** for indexed strip topologies is initially unsupported:
      - D3D11 encodes strip restart in the index buffer as `0xFFFF` (u16) / `0xFFFFFFFF` (u32).
      - Until we implement restart-aware strip assembly in compute, the runtime must reject draws
        that use indexed `LINESTRIP`/`TRIANGLESTRIP` with restart indices.
    - output strip topologies are expanded into lists (`line_strip` → `line_list`, `triangle_strip` → `triangle_list`),
    - no layered rendering system values (`SV_RenderTargetArrayIndex`, `SV_ViewportArrayIndex`),
    - output ordering is implementation-defined unless we add a deterministic prefix-sum mode (affects
      strict `SV_PrimitiveID` expectations).

- **P2 (tessellation: HS/DS)**
  - Tessellation is staged by supported **domain / partitioning**:
    - **P2a:** `domain("tri")` with integer partitioning, conservative clamping of tess factors.
    - **P2b:** `domain("quad")` with integer partitioning.
    - **P2c:** `domain("isoline")`.
    - **P2d:** fractional partitioning + crack-free edge rules.

#### Capabilities required

GS/HS/DS emulation requires the underlying WebGPU device/backend to support:

- compute pipelines (and storage buffers/atomics)
- indirect draws (to consume the generated draw args)

#### How to debug GS issues

Geometry shader failures are often “silent” (nothing draws) because the expansion pass can legally
emit zero primitives. Recommended debug workflow:

1. Use the `sm4_dump` tool (GS-TOOL-016) on the DXBC to confirm:
   - declared input primitive type and output topology
   - `maxvertexcount`
   - input/output signatures (semantics + component masks)
2. Confirm the command stream is binding the expected GS/HS/DS shader handles (via the extended
   `BIND_SHADERS` packet, or via the legacy `reserved0` GS slot) and that stage-specific resources
   are being populated (via `stage_ex`).
3. Enable wgpu/WebGPU validation and inspect errors around the expansion compute pass and the
   subsequent indirect draw (binding visibility, usage flags, and out-of-bounds writes are common
   root causes).

### 1) AeroGPU ABI extensions for GS/HS/DS

These changes are designed to be **minor-version, forward-compatible**:
packets grow only by appending new fields, and existing `reserved*` fields are repurposed in a way
that keeps old drivers valid (they still write zeros).

#### 1.1) `stage_ex` in resource-binding opcodes

The legacy `enum aerogpu_shader_stage` is extended with `GEOMETRY = 3`, but most GS/HS/DS resource
bindings use a `stage_ex` extension so compute-shader bindings (`@group(2)`) and emulated-stage
bindings (`@group(3)`) do not trample each other.

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
(`@group(3)`), using a small `stage_ex` tag carried in the trailing reserved field.

This is implemented in the emulator-side protocol mirror as `AerogpuShaderStageEx` + helpers
`encode_stage_ex`/`decode_stage_ex` (see `emulator/protocol/aerogpu/aerogpu_cmd.rs`).

**CREATE_SHADER_DXBC encoding:**

- Legacy encoding: `stage` is `VERTEX/PIXEL/COMPUTE` and `reserved0 = 0`.
- Stage-ex encoding: set `stage = COMPUTE` and store the extended stage in `reserved0`:
  - GS: `reserved0 = GEOMETRY` (2) (alternative to legacy `stage = GEOMETRY` where supported)
  - HS: `reserved0 = HULL` (3)
  - DS: `reserved0 = DOMAIN` (4)

Hosts should treat unknown non-zero `reserved0` values as invalid for now (reserved for future
stages/extensions).

**Packet layouts that carry `stage_ex` (normative summary)**

For all of the following packets:

- the struct is `#pragma pack(push, 1)` packed,
- `hdr.size_bytes` must include the header + any trailing payload arrays, and
- `reserved0` is interpreted as `stage_ex` **only** when the legacy `shader_stage`/`stage` field
  equals `COMPUTE` and `reserved0 != 0`.

| Packet | Packed struct header | Trailing payload |
|---|---|---|
| `CREATE_SHADER_DXBC` | `aerogpu_cmd_create_shader_dxbc { shader_handle, stage, dxbc_size_bytes, reserved0(stage_ex) }` | `dxbc_bytes[dxbc_size_bytes]` |
| `SET_TEXTURE` | `aerogpu_cmd_set_texture { shader_stage, slot, texture, reserved0(stage_ex) }` | none |
| `SET_SAMPLERS` | `aerogpu_cmd_set_samplers { shader_stage, start_slot, sampler_count, reserved0(stage_ex) }` | `aerogpu_handle_t samplers[sampler_count]` |
| `SET_CONSTANT_BUFFERS` | `aerogpu_cmd_set_constant_buffers { shader_stage, start_slot, buffer_count, reserved0(stage_ex) }` | `aerogpu_constant_buffer_binding bindings[buffer_count]` |
| `SET_SHADER_RESOURCE_BUFFERS` | `aerogpu_cmd_set_shader_resource_buffers { shader_stage, start_slot, buffer_count, reserved0(stage_ex) }` | `aerogpu_shader_resource_buffer_binding bindings[buffer_count]` |
| `SET_UNORDERED_ACCESS_BUFFERS` | `aerogpu_cmd_set_unordered_access_buffers { shader_stage, start_slot, uav_count, reserved0(stage_ex) }` | `aerogpu_unordered_access_buffer_binding bindings[uav_count]` |
| `SET_SHADER_CONSTANTS_F` | `aerogpu_cmd_set_shader_constants_f { stage, start_register, vec4_count, reserved0(stage_ex) }` | `float data[vec4_count * 4]` |

Implementers should copy the exact struct definitions (and sizes) from
`drivers/aerogpu/protocol/aerogpu_cmd.h` (source of truth). The table above exists so readers can
understand the extension pattern without having to jump to the header.

**Definition (matches DXBC program type values):**

```c
// New: used when binding resources for GS/HS/DS (and optionally compute).
//
// Values match DXBC program-type IDs (`D3D10_SB_PROGRAM_TYPE` / `D3D11_SB_PROGRAM_TYPE`):
//   0 = Pixel, 1 = Vertex, 2 = Geometry, 3 = Hull, 4 = Domain, 5 = Compute.
enum aerogpu_shader_stage_ex {
   AEROGPU_SHADER_STAGE_EX_PIXEL    = 0,
   AEROGPU_SHADER_STAGE_EX_VERTEX   = 1,
   AEROGPU_SHADER_STAGE_EX_GEOMETRY = 2,
   AEROGPU_SHADER_STAGE_EX_HULL     = 3,
   AEROGPU_SHADER_STAGE_EX_DOMAIN   = 4,
   AEROGPU_SHADER_STAGE_EX_COMPUTE  = 5,
};

// Note: in the *binding commands* described here, `stage_ex = 0` is treated as the legacy/default
// “no stage_ex” value (because old guests always write 0 into reserved fields). As a result, the
// DXBC program-type value `0 = Pixel` is not used via this extension; VS/PS continue to bind via
// the legacy `shader_stage` field.

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
- `stage_ex` encoding is enabled by setting `shader_stage = COMPUTE` and a non-zero `stage_ex`:
  - GS resources: `shader_stage = COMPUTE`, `stage_ex = GEOMETRY` (2)
  - HS resources: `shader_stage = COMPUTE`, `stage_ex = HULL`     (3)
  - DS resources: `shader_stage = COMPUTE`, `stage_ex = DOMAIN`   (4)
  - Other values (`stage_ex = VERTEX/PIXEL/COMPUTE`) are reserved and should not be used in binding
    commands.

**GS note:** because `enum aerogpu_shader_stage` includes `GEOMETRY = 3`, GS resource bindings may be
encoded either as:

- `shader_stage = GEOMETRY`, `stage_ex = 0` (direct/legacy GS encoding), or
- `shader_stage = COMPUTE`, `stage_ex = GEOMETRY` (uniform “stage_ex” encoding shared with HS/DS).

Implementations should accept both. Producers should prefer the `stage_ex` encoding for consistency
across GS/HS/DS, but must ensure they do not accidentally clobber CS bindings on hosts that do not
support the extension.

The host maintains separate binding tables for CS vs GS/HS/DS so that compute dispatch and
graphics-tess/GS pipelines do not trample each other’s bindings. At the WGSL interface level this
maps to distinct bind groups:

- CS uses `@group(2)`.
- GS/HS/DS use `@group(3)`.

#### 1.2) Extended `BIND_SHADERS` packet layout

`AEROGPU_CMD_BIND_SHADERS` is extended by appending `gs/hs/ds` handles after the existing payload.

Compatibility note: the legacy 24-byte packet already has a trailing `reserved0` field. In the
canonical ABI this field is repurposed as the **geometry shader (GS) handle**:

- `reserved0 == 0` → GS unbound
- `reserved0 != 0` → GS is bound to handle `reserved0`

The extended layout appends `{gs, hs, ds}` as explicit trailing fields. When using the extended
layout, producers SHOULD set `reserved0 = 0` and treat the appended `gs` field as authoritative.

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
   uint32_t reserved0;               // legacy GS handle (0 = unbound; non-zero = GS)

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
- Adjacency/patch topologies require the compute-expansion pipeline (GS/HS/DS emulation). Until
   that is implemented, the direct render path rejects these topologies at draw time.

### 2) Compute-expansion runtime pipeline

#### 2.1) When the expansion pipeline triggers

A draw uses compute expansion when **any** of the following are true:

- A **GS** shader is bound (`gs != 0`).
- A **HS** or **DS** shader is bound (`hs != 0` or `ds != 0`).

In the fully-general design, adjacency and patchlist topologies also route through this path even
if GS/HS/DS are unbound (so the runtime can surface deterministic validation/errors and implement
fixed-function tessellation semantics). Today, adjacency and patchlist topologies are accepted by
`SET_PRIMITIVE_TOPOLOGY` but rejected at draw time until the emulation kernels land.

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

For non-patch topologies, the number of *input primitives* (`input_prim_count`) is:

| Topology | Vertices consumed | Primitive count |
|---|---:|---:|
| `POINTLIST` | 1 / prim | `input_vertex_invocations` |
| `LINELIST` | 2 / prim | `input_vertex_invocations / 2` |
| `LINESTRIP` | N | `max(0, input_vertex_invocations - 1)` |
| `TRIANGLELIST` | 3 / prim | `input_vertex_invocations / 3` |
| `TRIANGLESTRIP` | N | `max(0, input_vertex_invocations - 2)` |
| `LINELIST_ADJ` | 4 / prim | `input_vertex_invocations / 4` |
| `LINESTRIP_ADJ` | `2*prim + 2` | `max(0, (input_vertex_invocations.saturating_sub(2)) / 2)` |
| `TRIANGLELIST_ADJ` | 6 / prim | `input_vertex_invocations / 6` |
| `TRIANGLESTRIP_ADJ` | `2*prim + 4` | `max(0, (input_vertex_invocations.saturating_sub(4)) / 2)` |

Rules:

- Any leftover vertices that don’t form a full primitive are ignored (matching D3D behavior).
- `*_ADJ` topologies require a GS that consumes adjacency (`lineadj`/`triadj`); otherwise the draw is
  invalid.
- **Primitive restart (indexed strip topologies):** for `LINESTRIP`/`TRIANGLESTRIP` with indexed
  draws, D3D11 uses a special index value to restart the strip (`0xFFFF` for u16 indices,
  `0xFFFFFFFF` for u32 indices). The simple formulas above assume there are no restart indices. For
  initial bring-up, treat any draw that contains restart indices as unsupported/invalid; a full
  implementation will need a restart-aware strip assembly path (either a preprocessing pass that
  expands strips into lists, or per-primitive bounds checks in the assembly stage).

For patchlist topologies:

- `control_points = topology - 32` (since `PATCHLIST_1 = 33`, …, `PATCHLIST_32 = 64`)
- `patch_count = input_vertex_invocations / control_points`
  - leftover indices/vertices are ignored.

Patchlist draws are invalid unless both HS and DS are bound.

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

1. **VS-out (`vs_out`)**
    - Purpose: stores vertex shader outputs (control points) for the draw, consumable by HS/GS.
    - Usage: `STORAGE` (written/read by compute).
    - Layout: `array<ExpandedVertex>` (see below).
    - Element count: `input_vertex_invocations * instance_count`.

2. **Tessellation-out (`tess_out_vertices`, `tess_out_indices`)**
    - Purpose: stores post-DS vertices + optional indices.
    - Usage (vertices): `STORAGE | VERTEX`
    - Usage (indices): `STORAGE | INDEX`
    - Index element type (baseline): `u32` (`wgpu::IndexFormat::Uint32`). A future optimization may
      choose `u16` when the expanded vertex count is known to fit.

3. **GS-out (`gs_out_vertices`, `gs_out_indices`)**
    - Purpose: stores post-GS vertices + indices suitable for final rasterization.
    - Usage (vertices): `STORAGE | VERTEX`
    - Usage (indices): `STORAGE | INDEX`
    - Index element type (baseline): `u32` (`wgpu::IndexFormat::Uint32`).
    - Capacity sizing: derived from `input_prim_count * instance_count * gs_maxvertexcount`, plus
      additional expansion when emitting list primitives without an index buffer (see below).

4. **Indirect args (`indirect_args`)**
    - Purpose: written by compute, consumed by render pass as indirect draw parameters.
    - Usage: `STORAGE | INDIRECT`

5. **Counters (`counters`)**
    - Purpose: atomic counters used during expansion (output vertex count, output index count,
      overflow flags).
    - Usage: `STORAGE` (atomics) and optionally `COPY_SRC` for debugging readback.

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

The baseline bring-up path should prefer (1). In that case, the worst-case per-primitive output
vertex bound (for sizing `out_max_vertices`) is:

| GS declared output | Max `EmitVertex` per input prim | Max list vertices per input prim |
|---|---:|---:|
| `point` | `M` | `M` |
| `line_strip` | `M` | `2 * max(0, M - 1)` |
| `triangle_strip` | `M` | `3 * max(0, M - 2)` |

Where `M = gs_maxvertexcount` from the GS bytecode.

So a conservative capacity is:

```
out_max_vertices =
  input_prim_count * instance_count * max_list_vertices_per_input_prim
```

If using strategy (2), then:

- `out_max_vertices = input_prim_count * instance_count * M`
- `out_max_indices = input_prim_count * instance_count * max_list_vertices_per_input_prim`

**Expanded vertex layout (concrete):**

For compatibility with signature-driven stage linking *and* to preserve integer/float bit patterns,
expansion outputs store the same logical interface that the pixel shader consumes, but encoded as
raw 32-bit lanes:

- `pos_bits`: `vec4<u32>` containing IEEE-754 `f32` bits for `SV_Position`
- `varyings[i]`: `vec4<u32>` for the *i*th linked varying, where varyings are ordered by ascending
  `@location` number (i.e. `varying_locations` is a sorted list and `varyings[i]` corresponds to
  `@location(varying_locations[i])`).

One concrete layout:

```wgsl
// One entry per expanded vertex (post-VS, post-DS, or post-GS depending on which scratch buffer).
struct ExpandedVertex {
  pos_bits: vec4<u32>;
  varyings: array<vec4<u32>, VARYING_COUNT>;
}
```

Where `VARYING_COUNT` is the number of linked varyings in the VS/GS/DS → PS signature intersection.
The passthrough vertex shader then:

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
render pipeline by binding it as a WebGPU **vertex buffer**.

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
      - When drawing indexed expansion output, bind the generated index buffer as
        `wgpu::IndexFormat::Uint32` (baseline).

**Passthrough VS strategy (concrete)**

The final render stage uses a small **passthrough vertex shader** plus the original pixel shader.
The passthrough VS uses normal WebGPU vertex inputs (no storage-buffer reads in the vertex stage)
and forwards attributes from the expansion output vertex buffer.

The current in-tree executor uses a built-in passthrough VS template
`EXPANDED_DRAW_PASSTHROUGH_VS_WGSL` (and generates trimmed/depth-clamped variants on demand; see
`get_or_create_render_pipeline_for_expanded_draw` in
`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`). Conceptually:

```wgsl
struct VsIn {
  @location(0) v0: vec4<f32>,
  @location(1) v1: vec4<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(1) o1: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
  var out: VsOut;
  out.pos = input.v0;
  out.o1 = input.v1;
  return out;
}
```

Notes:

- The executor links the expanded VS and the application PS by trimming unused PS inputs / VS
  outputs. If the PS reads a varying location the passthrough VS cannot provide, the draw fails with
  a clear error.
- This path is limited by WebGPU’s vertex input limits (`max_vertex_attributes` and the highest used
  `@location`). When exceeded, the executor fails with a clear “GS passthrough” error.
- The passthrough VS has no bindings; however the render pipeline layout must still include the PS
  bind group(s) (typically `@group(1)`). Implementations may include an empty `@group(0)` or the
  original VS layout for cache compatibility, but no VS resources are required by the passthrough VS
  itself.
- Implementation note: the in-tree placeholder expansion prepass currently writes `vec4<f32>`
  attributes (`pos` + `o1`). When switching to the bit-preserving `ExpandedVertex` layout described
  above, update the passthrough VS template and vertex buffer formats (e.g. use `Uint32x4` +
  `bitcast`).

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

- D3D resources live in `@group(3)` (the reserved `stage_ex` group) and use the same binding number
  scheme as other stages:
  - `b#` (cbuffers) → `@binding(BINDING_BASE_CBUFFER + slot)`
  - `t#` (SRVs)     → `@binding(BINDING_BASE_TEXTURE + slot)`
  - `s#` (samplers) → `@binding(BINDING_BASE_SAMPLER + slot)`
  - `u#` (UAVs, SM5) → `@binding(BINDING_BASE_UAV + slot)` (where supported)
- Resource-binding opcodes specify the logical stage via `stage_ex` so the runtime can maintain
  separate tables for CS vs GS/HS/DS (and so the WGSL interface uses distinct groups 2 vs 3).

#### 3.2) Internal bind groups and reserved bindings

Expansion compute pipelines require additional buffers that are not part of the D3D binding model
(vertex pulling inputs, scratch outputs, counters, indirect args).

Implementation note: the layout described below is the **target** binding scheme. The current
executor’s placeholder compute-prepass still uses a separate bind group layout for its output
buffers. Vertex pulling already uses the reserved expansion-internal binding range (starting at
`BINDING_BASE_INTERNAL = 256`) within `VERTEX_PULLING_GROUP` (`@group(3)`). Future work is to unify
all emulation kernels on the shared internal layout.

These are not part of the D3D binding model, so they use a reserved binding-number range starting at
`BINDING_BASE_INTERNAL = 256`. In the baseline design they live in the same bind group as GS/HS/DS
resources (`@group(3)`), but implementations may temporarily place them in a dedicated internal group
(`@group(4)`, `BIND_GROUP_INTERNAL_EMULATION`) as long as the device supports at least 5 bind groups.

Let:

- D3D resource bindings occupy:
  - `@binding(0..BINDING_BASE_TEXTURE)` for cbuffers
  - `@binding(BINDING_BASE_TEXTURE..BINDING_BASE_SAMPLER)` for SRVs
  - `@binding(BINDING_BASE_SAMPLER..BINDING_BASE_UAV)` for samplers
  - `@binding(BINDING_BASE_UAV..BINDING_BASE_UAV + MAX_UAV_SLOTS)` for UAVs
- Expansion-internal bindings start at `BINDING_BASE_INTERNAL = 256`.

Within the chosen internal group, the expansion-internal bindings are reserved and stable so the
runtime can share common helper WGSL across VS/GS/HS/DS compute variants:

- `@binding(256)`: `ExpandParams` (uniform/storage; draw parameters + topology info)
- `@binding(257..=264)`: vertex buffers `vb0..vb7` as read-only storage (after slot compaction)
- `@binding(265)`: index buffer (read-only storage; absent → bind dummy)
- `@binding(266)`: `vs_out` (read_write storage)
- `@binding(267)`: `tess_out_vertices` (read_write storage)
- `@binding(268)`: `tess_out_indices` (read_write storage)
- `@binding(269)`: `gs_out_vertices` (read_write storage)
- `@binding(270)`: `gs_out_indices` (read_write storage)
- `@binding(271)`: `indirect_args` (read_write storage)
- `@binding(272)`: `counters` (read_write storage; atomics)

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
  uint32_t draw_kind;   // 0 = Draw, 1 = DrawIndexed
  uint32_t topology;    // enum aerogpu_primitive_topology (including adj/patchlist extensions)
  uint32_t vertex_count;
  uint32_t index_count; // 0 for Draw

  uint32_t instance_count;
  uint32_t first_vertex;
  uint32_t first_index;    // 0 for Draw
  uint32_t first_instance;

  int32_t  base_vertex;    // 0 for Draw
  uint32_t index_format;   // enum aerogpu_index_format (valid only for DrawIndexed)
  uint32_t index_offset_bytes; // IA index-buffer binding offset in bytes
  uint32_t expanded_vertex_stride_bytes;

  uint32_t out_max_vertices; // capacity of the current output vertex buffer (elements)
  uint32_t out_max_indices;  // capacity of the current output index buffer (elements; 0 if unused)
  uint32_t _pad0;
  uint32_t _pad1;

  // Compact IA vertex-buffer bindings (after slot compaction).
  // Each entry corresponds to vbN in `@binding(257 + N)`.
  struct AerogpuExpandVertexBuffer vb[8];
};
```

WGSL-side, this is typically declared as:

```wgsl
struct ExpandParams { /* same fields */ }
// Bind group index is `3` in the baseline design (shared with GS/HS/DS resources). Implementations
// using a dedicated internal group instead use `@group(4)`.
@group(3) @binding(256) var<uniform> params: ExpandParams;
```

**`counters` layout (concrete; `@binding(272)`)**

The counters buffer is written by expansion passes and read when finalizing indirect args:

```wgsl
struct ExpandCounters {
  out_vertex_count: atomic<u32>;
  out_index_count: atomic<u32>;
  overflow: atomic<u32>; // 0/1 (set when any pass exceeds out_*_max)
  _pad0: u32;
}
// Bind group index is `3` in the baseline design (shared with GS/HS/DS resources). Implementations
// using a dedicated internal group instead use `@group(4)`.
@group(3) @binding(272) var<storage, read_write> counters: ExpandCounters;
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

**Initialization requirements**

Before running expansion for a draw, the runtime MUST initialize the per-draw scratch state:

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

**Pixel-compare tests (Rust):**

Add new `aero-d3d11` executor tests that render to an offscreen RT and compare readback pixels:

- `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_*.rs`
  - Example: `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs`
- `crates/aero-d3d11/tests/aerogpu_cmd_tessellation_*.rs`

Each test should:

1. Upload VS/PS (+ GS/HS/DS) fixtures.
2. Bind topology (including adjacency/patchlist where relevant).
3. Issue a draw that exercises the expansion path.
4. Read back the render target and compare to a tiny reference image (or a simple expected pattern).

When GS support lands, update the existing “ignore GS payloads” robustness test
(`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_ignore.rs`) to reflect the new behavior (GS
is no longer ignored when bound, whether via the legacy `reserved0` slot or the extended
`BIND_SHADERS` packet).

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
10. **Adjacency input**: `TRIANGLELIST_ADJ` + GS consumes adjacency vertices; validates adjacency
   topology decode + binding (even if the initial GS subset does not yet implement adjacency ops).

### P2 scenes

11. **Compute blur**: run CS to blur a texture then render it
12. **UAV write**: CS writes to structured buffer; PS reads and visualizes
13. **Tess P2a (tri domain, integer)**: `PATCHLIST_3` + simple HS/DS produces a subdivided triangle
    grid (color = `SV_DomainLocation`), validates patchlist topology + HS/DS execution.
14. **Tess P2b (quad domain, integer)**: `PATCHLIST_4` + simple HS/DS produces a subdivided quad.

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
