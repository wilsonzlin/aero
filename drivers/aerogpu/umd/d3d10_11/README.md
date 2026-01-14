# AeroGPU Windows 7 D3D10/11 User-Mode Driver (UMD)

This directory contains the **Direct3D 10 / Direct3D 11 Windows 7 SP1 user-mode driver** for AeroGPU.

The UMD is responsible for translating the D3D10DDI/D3D11DDI calls made by the D3D runtime into the **AeroGPU command stream** defined in `drivers/aerogpu/protocol/`.

This UMD targets only the **command stream** ABI (`drivers/aerogpu/protocol/aerogpu_cmd.h`). The kernel-mode driver (KMD)
owns the submission transport and supports both the versioned (`aerogpu_pci.h` + `aerogpu_ring.h`) and legacy
(`legacy/aerogpu_protocol_legacy.h`) device ABIs, auto-detected via MMIO magic; see `drivers/aerogpu/kmd/README.md`.
(The in-tree Win7 driver package binds only to the versioned device by default; legacy uses `drivers/aerogpu/packaging/win7/legacy/`.)

## Status / scope

This section started as the initial milestone scope; it is kept updated as feature coverage expands.

This UMD is still bring-up oriented and targets **D3D_FEATURE_LEVEL_10_0** (FL10\_0) behavior on Windows 7 (WDDM 1.1).

### Supported (summary)

Feature matrix for the Win7 WDK-backed UMDs:

| Feature | D3D10 (`src/aerogpu_d3d10_umd_wdk.cpp`) | D3D10.1 (`src/aerogpu_d3d10_1_umd_wdk.cpp`) | D3D11 (`src/aerogpu_d3d11_umd_wdk.cpp`) |
| --- | --- | --- | --- |
| MRT (multiple render targets) | Up to `AEROGPU_MAX_RENDER_TARGETS` (8)\* | Up to `AEROGPU_MAX_RENDER_TARGETS` (8)\* | Up to `AEROGPU_MAX_RENDER_TARGETS` (8)\* |
| Pipeline state encoding (blend / raster / depth) | **Supported** | **Supported** | **Supported** |
| Vertex buffer binding | **Multiple slots** supported (`StartSlot/NumBuffers` forwarded) | **Multiple slots** supported (`StartSlot/NumBuffers` forwarded) | **Multiple slots** supported (`StartSlot/NumBuffers` forwarded) |
| Constant buffers | VS/PS/GS supported (14 slots, whole-buffer binding) | VS/PS/GS supported (14 slots, whole-buffer binding) | VS/PS/GS/CS supported (14 slots, `{FirstConstant, NumConstants}` ranges supported) |
| Samplers | VS/PS/GS supported (16 slots; `CREATE_SAMPLER` + `SET_SAMPLERS`) | VS/PS/GS supported (16 slots; `CREATE_SAMPLER` + `SET_SAMPLERS`) | VS/PS/GS/CS supported (16 slots; basic filter/address modes) |
| Geometry shaders (GS) | **Supported (partial)**: create+bind (legacy: handle in `aerogpu_cmd_bind_shaders.reserved0`; newer streams may append `{gs,hs,ds}` after the 24-byte prefix—appended handles are authoritative and `reserved0` may optionally mirror `gs`) | **Supported (partial)**: create+bind (legacy: handle in `aerogpu_cmd_bind_shaders.reserved0`; newer streams may append `{gs,hs,ds}` after the 24-byte prefix—appended handles are authoritative and `reserved0` may optionally mirror `gs`) | **Supported (partial)**: create+bind+stage bindings; host runs compute-prepass emulation (synthetic fallback + minimal SM4 GS DXBC execution for a small set of IA input topologies: `PointList`/`LineList`/`TriangleList`/`LineListAdj`/`TriangleListAdj`) |
| Compute (CS) + UAV buffers | — | — | **Supported (partial)**: CS shaders + `AEROGPU_CMD_DISPATCH`; UAV **buffers** only (8 slots; no UAV textures / OM UAV binding) |

\* All UMDs (D3D10 / D3D10.1 / D3D11) preserve the runtime-provided RTV slot count/list when emitting `SET_RENDER_TARGETS`: `color_count` reflects the runtime-provided slot count, clamped to `AEROGPU_MAX_RENDER_TARGETS` (8). `NULL` entries within `[0, color_count)` are valid and are encoded as `colors[i] = 0` (gaps are preserved).

### Implemented

- Device + immediate context (FL10_0)
- Buffers + Texture2D resources
  - Texture2D **mip chains + array layers** (`MipLevels = 0` → full chain), including initial-data upload + subresource layout packing for guest-backed allocations
  - 16-bit packed formats (`B5G6R5_UNORM`, `B5G5R5A1_UNORM`)
  - Block-compressed formats (BC1/BC2/BC3/BC7) and explicit sRGB variants are ABI-gated (ABI 1.2+; see `aerogpu_umd_private_v1.device_abi_version_u32`). On older ABIs, sRGB DXGI formats are mapped to UNORM for command-stream compatibility; BC formats are rejected.
- Shaders (DXBC payload passthrough):
  - D3D10/D3D10.1: VS/PS/GS
  - D3D11: VS/PS/GS/CS (GS binding triggers compute-prepass path; a small translated SM4 GS subset can execute for a limited set of IA input topologies: `PointList`/`LineList`/`TriangleList`/`LineListAdj`/`TriangleListAdj` (`Draw` + `DrawIndexed`); other IA input topologies (strip inputs, strip-adjacency, patchlists) still fall back to synthetic expansion; see “Geometry shaders (GS)” below)
- Input layout + vertex/index buffers, primitive topology
- Shader binding tables:
  - D3D10: VS/PS/GS constant buffers, shader-resource views, samplers (whole-buffer constant-buffer binding)
  - D3D10.1: VS/PS/GS constant buffers, shader-resource views, samplers (whole-buffer constant-buffer binding)
  - D3D11: VS/PS/GS/CS constant buffers (supports `{FirstConstant, NumConstants}` ranges), shader-resource views, samplers; CS UAV buffers
- Render target + depth-stencil binding (MRT up to `AEROGPU_MAX_RENDER_TARGETS`), Clear, Draw/DrawIndexed
- Viewport + scissor (protocol supports a **single** viewport/scissor; non-trivial arrays are surfaced via `E_NOTIMPL` and applied best-effort as slot 0)
- D3D11 compute:
  - `CreateComputeShader` + `CsSet*` bindings + `Dispatch`
  - Best-effort indirect execution (reads the arg buffer from the UMD's CPU shadow storage; does not support GPU-generated indirect args):
    - `DispatchIndirect`
    - `DrawInstancedIndirect` / `DrawIndexedInstancedIndirect`
  - Buffer UAV utilities:
    - `ClearUnorderedAccessViewUint` / `ClearUnorderedAccessViewFloat` (buffer UAVs only)
    - Best-effort `CopyStructureCount` (no real UAV counter tracking yet; forwards `initial_count` when available)
- Resource updates + readback:
  - `Map`/`Unmap` for buffers and Texture2D subresources (uploads via `AEROGPU_CMD_RESOURCE_DIRTY_RANGE` / `AEROGPU_CMD_UPLOAD_RESOURCE`)
  - Staging readback uses `AEROGPU_CMD_COPY_*` + `AEROGPU_COPY_FLAG_WRITEBACK_DST` when the host exposes `AEROGPU_FEATURE_TRANSFER` (ABI 1.1+)
- Pipeline state **encoding** into the command stream:
  - D3D10: `AEROGPU_CMD_SET_BLEND_STATE`, `AEROGPU_CMD_SET_RASTERIZER_STATE`, `AEROGPU_CMD_SET_DEPTH_STENCIL_STATE`
  - D3D10.1: `AEROGPU_CMD_SET_BLEND_STATE`, `AEROGPU_CMD_SET_RASTERIZER_STATE`, `AEROGPU_CMD_SET_DEPTH_STENCIL_STATE`
  - D3D11: `AEROGPU_CMD_SET_BLEND_STATE`, `AEROGPU_CMD_SET_RASTERIZER_STATE`, `AEROGPU_CMD_SET_DEPTH_STENCIL_STATE`
- DXGI swapchain bring-up: `Present` + backbuffer identity rotation (`RotateResourceIdentities`), with presentation via `AEROGPU_CMD_PRESENT` (sync interval 0 vs non-zero)

### Not yet supported / requires protocol changes

- **Subresource view selection** (SRV/RTV/DSV mip level + array slice): the UMDs currently only support “full-resource” views (no mip/array slicing; view descriptors must select mip 0 and cover the full resource when accepted) and bindings resolve to the underlying texture handle only. Supporting arbitrary per-view subresource selection requires protocol representation of “views” (or subresource selectors) rather than just raw texture handles.
- **Texture UAVs + OM UAV binding**: the D3D11 UMD supports **buffer** UAVs in the CS stage (`AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS`), but does not support UAVs over textures, and rejects OM UAV binding (`OMSetRenderTargetsAndUnorderedAccessViews`) with `E_NOTIMPL`. Full UAV support requires protocol view/subresource selectors and a texture-UAV representation.
- **Multiple viewports/scissor rects**: the protocol encodes only one viewport and one scissor rect. The UMD validates that any additional entries are identical/disabled; otherwise it reports `E_NOTIMPL`.
- **DXGI format expansion** beyond the protocol’s current `enum aerogpu_format` list: only formats representable in the protocol can be encoded. The supported DXGI subset (including ABI-gated sRGB/BC policies) is centralized in `src/aerogpu_dxgi_format.h`. Adding more DXGI formats still requires extending `drivers/aerogpu/protocol/aerogpu_pci.h` + host support.
- Stencil ops are protocol-limited: the current `aerogpu_depth_stencil_state` only carries **stencil enable + masks**; it does **not** encode stencil funcs/ops (or separate front/back face state).
- Blend state is protocol-limited:
  - The protocol encodes only a single global blend state. D3D10/10.1/11 blend state objects may describe **per-render-target** blend state; the UMD rejects non-uniform per-RT blend state (`E_NOTIMPL`) rather than silently misrendering.
  - Alpha-to-coverage is not representable and is rejected (`E_NOTIMPL`).
  - Only `{Zero, One, SrcAlpha, InvSrcAlpha, DestAlpha, InvDestAlpha, Constant, InvConstant}` blend factors (and a limited set of blend ops) are representable; others are rejected at `CreateBlendState` time (`E_NOTIMPL`).

### Still stubbed / known gaps

- Geometry shaders (GS):
  - D3D10 / D3D10.1: `CreateGeometryShader` + `GsSetShader` (and GS resource bindings: `GsSetConstantBuffers`, `GsSetShaderResources`, `GsSetSamplers`) are forwarded into the command stream.
    - Legacy compat: GS handle carried via `aerogpu_cmd_bind_shaders.reserved0`.
    - Forward-compat: the protocol also supports an append-only `BIND_SHADERS` extension that appends `{gs,hs,ds}` after the base 24-byte packet. When present, the appended handles are authoritative; producers may optionally mirror `gs` into `reserved0` for best-effort legacy compatibility (if mirrored, it should match the appended `gs`).
  - D3D11:
    - `CreateGeometryShader` + `GsSetShader` are forwarded into the command stream (GS handle carried via `aerogpu_cmd_bind_shaders.reserved0` for legacy compat).
    - GS stage resource binding DDIs (`GsSetConstantBuffers`, `GsSetShaderResources`, `GsSetSamplers`) emit binding packets; the host tracks these bindings for GS compute-emulation. Today, draws using a supported IA topology subset (`PointList`/`LineList`/`TriangleList`/`LineListAdj`/`TriangleListAdj`) can execute translated SM4 GS DXBC. If GS translation fails, draws with that GS bound currently return a clear error; if translation succeeds but the draw is outside the translated-GS execution paths, the executor falls back to synthetic expansion (guest GS DXBC does not execute).
  - Host/WebGPU execution:
    - WebGPU has no geometry stage; AeroGPU uses a **compute prepass + indirect draw** path when GS/HS/DS emulation is required.
    - Prepass implementations:
      - A deterministic synthetic-expansion compute prepass used for bring-up/fallback (see `GEOMETRY_PREPASS_CS_WGSL` / `GEOMETRY_PREPASS_CS_VERTEX_PULLING_WGSL` in `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
      - GS prepass paths that can execute a translated SM4 GS DXBC subset for a small set of IA input topologies (`PointList`/`LineList`/`TriangleList`/`LineListAdj`/`TriangleListAdj`) (bring-up limitations apply: the VS-as-compute feeding path is still minimal; see `docs/graphics/geometry-shader-emulation.md`).
    - A minimal SM4 GS DXBC→WGSL compute translator exists in `crates/aero-d3d11/src/runtime/gs_translate.rs` and is partially wired into the executor:
      - `CREATE_SHADER_DXBC` attempts to translate GS DXBC into a compute prepass.
      - Eligible draws can execute the translated prepass when translation succeeds (see `exec_geometry_shader_prepass_*` in `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`).
      - Strip-cut/RestartStrip expansion helpers live in `crates/aero-d3d11/src/runtime/strip_to_list.rs`.
    - The command stream exposes an ABI extension for extended D3D11 stages (`stage_ex`; see `enum aerogpu_shader_stage_ex` in `drivers/aerogpu/protocol/aerogpu_cmd.h`). The host executor accepts both the direct `AEROGPU_SHADER_STAGE_GEOMETRY` (`stage = 3`) encoding and the `stage_ex` encoding:
      - Preferred: `shader_stage = GEOMETRY`, `reserved0 = 0`
      - Compatibility (`stage_ex`): `shader_stage = COMPUTE`, `reserved0 = AEROGPU_SHADER_STAGE_EX_GEOMETRY`
      - HS/DS require `stage_ex`
    - Win7 GS tests:
      - `drivers/aerogpu/tests/win7/d3d11_geometry_shader_smoke`
      - `drivers/aerogpu/tests/win7/d3d11_geometry_shader_restart_strip`
      - Host-side tests live under `crates/aero-d3d11/tests/` (run via `cargo test -p aero-d3d11`).
    - Translator-backed GS prepass supported subset (covered by `crates/aero-d3d11/tests/gs_translate.rs`; executed for the supported IA input topology subset):
      - End-to-end execution path: `Draw` and `DrawIndexed` for `PointList`/`LineList`/`TriangleList` and adjacency list variants (`LineListAdj`/`TriangleListAdj`).
      - Remaining work (translated-GS execution): strip input topologies (`LineStrip`/`TriangleStrip`) and strip-adjacency variants (`LineStripAdj`/`TriangleStripAdj`) (these currently route through synthetic expansion).
      - Output (end-to-end): GS output topology `pointlist`/`linestrip`/`triangle_strip` (stream 0). Strip output is expanded into indexed list topologies for `draw_indexed_indirect` (and `pointlist` is rendered as `PointList`).
      - Shader instructions/operands: a small SM4 subset (see `docs/graphics/geometry-shader-emulation.md`), including `emit`/`cut`, basic ALU, structured control flow (`if`/`loop`/`break`/`continue`, etc), and a small set of **read-only** resource ops:
        - Texture2D: `sample`/`sample_l`/`ld`/`resinfo`
        - SRV buffers: `ld_raw`/`ld_structured`/`bufinfo`
    - Design notes: [`docs/graphics/geometry-shader-emulation.md`](../../../../docs/graphics/geometry-shader-emulation.md)
  - Known unsupported / not yet implemented:
    - Stream-output (SO):
      - D3D11 accepts `CreateGeometryShaderWithStreamOutput`, but ignores the stream-output declaration; binding real SO targets (`SOSetTargets`) reports `E_NOTIMPL`.
      - D3D10 / D3D10.1 accept `CreateGeometryShaderWithStreamOutput` (the stream-output declaration is ignored; it behaves like `CreateGeometryShader`). Binding real SO targets (`SoSetTargets`) reports `E_NOTIMPL` (unbind is a no-op), so stream-out is not implemented.
    - Multi-stream GS output (`emit_stream` / `cut_stream`); non-zero stream indices are not supported.
    - Most real-world SM4 GS shaders are not supported yet (broader resource access beyond the current read-only Texture2D/SRV buffer subset, additional opcodes/operands, and IO/system-value coverage beyond the translated subset).
- Tessellation (HS/DS) and other D3D11 features outside the implemented subset should fail cleanly (`E_NOTIMPL` / `SetErrorCb`).

Unsupported functionality must fail cleanly (returning `E_NOTIMPL` / `E_INVALIDARG`) rather than crashing or dereferencing null DDI function pointers.

Host-side unit tests (portable; no WDK required) for command-stream encoding and validation live under
`drivers/aerogpu/umd/d3d10_11/tests/` (see `CMakeLists.txt` for the full list of `aerogpu_d3d10_11_*_tests`):

- `drivers/aerogpu/umd/d3d10_11/tests/map_unmap_tests.cpp` (CMake target: `aerogpu_d3d10_11_map_unmap_tests`; portable ABI build, no WDK required) covers Map/Unmap upload + staging readback, including mip/array layout (`TestGuestBackedTexture2DMipArray*`) and BC format paths (`Test*BcTexture*`).
  - Quick run (from repo root):
    - `cmake -S drivers/aerogpu/umd/d3d10_11/tests -B out/umd_d3d10_11_tests && cmake --build out/umd_d3d10_11_tests && ctest --test-dir out/umd_d3d10_11_tests -V`
- `drivers/aerogpu/umd/d3d10_11/tests/dxgi_format_tests.cpp` (CMake target: `aerogpu_d3d10_11_dxgi_format_tests`) covers DXGI→AeroGPU format mapping and ABI-gated sRGB/BC policies in `src/aerogpu_dxgi_format.h`.
- `drivers/aerogpu/umd/d3d10_11/tests/state_packets_tests.cpp` (CMake target: `aerogpu_d3d10_11_state_packets_tests`) covers fixed-function state packet encoding (`SET_BLEND_STATE`, `SET_RASTERIZER_STATE`, `SET_DEPTH_STENCIL_STATE`) and protocol invariants (size/alignment, default-state behavior).
- `drivers/aerogpu/umd/d3d10_11/tests/depth_stencil_state_tests.cpp` (CMake target: `aerogpu_d3d10_11_depth_stencil_state_tests`) covers depth-stencil state packet encoding (`SET_DEPTH_STENCIL_STATE`), including stencil mask propagation and forcing depth writes off when depth testing is disabled.
- `drivers/aerogpu/umd/d3d10_11/tests/blend_state_validation_tests.cpp` (CMake target: `aerogpu_d3d10_11_blend_state_validation_tests`) covers blend-state validation rules and protocol limits (non-uniform per-RT blend state rejected, alpha-to-coverage rejected, limited blend factors/ops).
- `drivers/aerogpu/umd/d3d10_11/tests/viewport_scissor_validation_tests.cpp` (CMake target: `aerogpu_d3d10_11_viewport_scissor_validation_tests`) covers single-viewport/scissor validation behavior (`E_NOTIMPL` surfaced for mismatched arrays; best-effort slot 0 is applied).
- `drivers/aerogpu/umd/d3d10_11/tests/render_targets_tests.cpp` (CMake target: `aerogpu_d3d10_11_render_targets_tests`) and `drivers/aerogpu/umd/d3d10_11/tests/mrt_tests.cpp` (CMake target: `aerogpu_d3d10_11_mrt_tests`) cover `SET_RENDER_TARGETS` packet encoding and MRT invariants (including slot/gap preservation).
- `drivers/aerogpu/umd/d3d10_11/tests/render_target_tests.cpp` (CMake target: `aerogpu_d3d10_11_render_target_tests`) is an end-to-end harness that opens an adapter/device (`OpenAdapter10`) and validates render-target binding behavior.
- `drivers/aerogpu/umd/d3d10_11/tests/gs_shader_packets_tests.cpp` (CMake target: `aerogpu_d3d10_11_gs_shader_packets_tests`) and `drivers/aerogpu/umd/d3d10_11/tests/gs_resource_packets_tests.cpp` (CMake target: `aerogpu_d3d10_11_gs_resource_packets_tests`) cover:
  - GS create/bind and geometry-stage resource binding packet encoding using the preferred direct `AEROGPU_SHADER_STAGE_GEOMETRY` encoding, and
  - forward-compat encoding for HS/DS via the `stage_ex` ABI extension plus the append-only extended `BIND_SHADERS` layout.
- Host-side tests live under `crates/aero-d3d11/tests/` (run via `cargo test -p aero-d3d11`), including command-stream execution smoke coverage for `AEROGPU_CMD_*` packets plus unit tests for GS translation (`runtime/gs_translate.rs`) and strip restart semantics (`runtime/strip_to_list.rs`).
- Command-stream/host validation for B5 formats, MRT, and state packets lives under `crates/aero-gpu/tests/` (run via `cargo test -p aero-gpu`)
  (for example: `aerogpu_d3d9_16bit_formats.rs`, `aerogpu_d3d9_clear_scissor.rs`, `aerogpu_d3d9_cmd_stream_state.rs`).

For a full “bring-up spec” (Win7 driver model overview, minimal D3D10DDI/D3D11DDI entrypoints to implement, swapchain behavior expectations, shader handling, and a test plan), see:

- [`docs/graphics/win7-d3d10-11-umd-minimal.md`](../../../../docs/graphics/win7-d3d10-11-umd-minimal.md)
- [`docs/graphics/win7-aerogpu-validation.md`](../../../../docs/graphics/win7-aerogpu-validation.md) (Win7 validation/stability checklist: TDR, vblank, perf baseline, dbgctl playbook)
- [`docs/windows/win7-wddm11-d3d10-11-umd-alloc-map.md`](../../../../docs/windows/win7-wddm11-d3d10-11-umd-alloc-map.md) (deprecated redirect; kept for link compatibility)
- [`docs/graphics/win7-d3d11ddi-function-tables.md`](../../../../docs/graphics/win7-d3d11ddi-function-tables.md) (DDI function-table checklist: REQUIRED vs stub for FL10_0)
- [`docs/graphics/win7-d3d10-11-umd-allocations.md`](../../../../docs/graphics/win7-d3d10-11-umd-allocations.md) (resource allocation contract: `CreateResource` → `pfnAllocateCb` + `D3DDDI_ALLOCATIONINFO`)
- [`docs/graphics/win7-d3d11-map-unmap.md`](../../../../docs/graphics/win7-d3d11-map-unmap.md) (`Map`/`Unmap` contract: `LockCb`/`UnlockCb`, DO_NOT_WAIT, staging readback sync)
- [`docs/graphics/win7-dxgi-swapchain-backbuffer.md`](../../../../docs/graphics/win7-dxgi-swapchain-backbuffer.md) (trace guide: swapchain backbuffer `CreateResource` parameters and allocation flags)
- [`docs/graphics/aerogpu-protocols.md`](../../../../docs/graphics/aerogpu-protocols.md) (protocol header overview: where `aerogpu_cmd.h` and `aerogpu_format` live)

## Bring-up tracing (Win7)

For early Win7 bring-up it is often useful to trace:

* which `pfnGetCaps` query types the runtime is requesting, and
* which D3D10DDI entrypoints are being called unexpectedly (NULL-vtable avoidance).

See:

* `docs/graphics/win7-d3d10-caps-tracing.md`

## Feature level strategy

The initial feature claim is **D3D_FEATURE_LEVEL_10_0**:

- D3D11 runtime compatibility (can create a D3D11 device at FL10_0)
- Avoids implementing SM5.0-only features (tessellation, etc.) early
- Matches the minimal pipeline required for a triangle sample

## Command stream mapping

This UMD emits `drivers/aerogpu/protocol/aerogpu_cmd.h` packets and references objects using **protocol resource handles** (`aerogpu_handle_t`), not an “allocation list index” model:

- Packets reference protocol objects via `resource_handle` / `buffer_handle` / `texture_handle` fields (`aerogpu_handle_t`), chosen by the UMD.
- When a resource is backed by guest memory, create packets may set `backing_alloc_id` (a stable `alloc_id`) and `backing_offset_bytes`. The `alloc_id` is resolved by looking it up in the optional per-submission `aerogpu_alloc_table` (`drivers/aerogpu/protocol/aerogpu_ring.h`), which maps `alloc_id → {gpa, size_bytes, flags}`. `backing_alloc_id` is a lookup key, not an index. `backing_alloc_id = 0` means “host allocated” (no guest backing).
- `aerogpu_handle_t` values are protocol object IDs; they are intentionally **not** WDDM allocation handles/IDs.

### Win7 submission invariant: allocation list drives the `alloc_table`

On Win7/WDDM 1.1, the KMD builds the per-submit `aerogpu_alloc_table` from the submission’s WDDM allocation list (`DXGK_ALLOCATIONLIST`), so the UMD must ensure:

- Any submission that includes packets requiring `alloc_id` resolution includes the corresponding **WDDM allocation handle** in the submit allocation list (so the KMD can emit the `alloc_id → gpa` mapping). This includes:
  - `CREATE_*` packets with `backing_alloc_id != 0`
  - `AEROGPU_CMD_RESOURCE_DIRTY_RANGE` for guest-backed resources (Map/Unmap upload paths)
  - `COPY_*` packets with `WRITEBACK_DST` (staging readback)
- Do not rely solely on “currently bound” state when building the list: these packets may be emitted while a resource is **not currently bound**, and still require the allocation to be listed for that submit.
- The allocation list also carries per-allocation **write intent** (`WriteOperation` bit; `DXGK_ALLOCATIONLIST::Flags.Value & 0x1`). The Win7 KMD propagates this into `aerogpu_alloc_entry.flags` as `AEROGPU_ALLOC_FLAG_READONLY` when an allocation is not written by the submission; the host rejects any guest-memory writeback (e.g. `COPY_* WRITEBACK_DST`) into READONLY allocations. UMDs must therefore ensure writeback destinations are marked writable for the submission.

The WDK-backed UMDs enforce this via `TrackWddmAllocForSubmitLocked()` in:

- `src/aerogpu_d3d10_umd_wdk.cpp`
- `src/aerogpu_d3d10_1_umd_wdk.cpp`
- `src/aerogpu_d3d11_umd_wdk.cpp`

Related lifetime rule: when destroying a **guest-backed** resource, emit `AEROGPU_CMD_DESTROY_RESOURCE` and flush/submit it before calling `pfnDeallocateCb` (so the submission does not reference a freed allocation handle).

The core emission happens in the WDK-facing UMD entrypoints
(`src/aerogpu_d3d10_1_umd_wdk.cpp`, `src/aerogpu_d3d11_umd_wdk.cpp`) and the shared
encoder/state tracker (`src/aerogpu_d3d10_11_internal.h`) by building a linear
command buffer consisting of:

```
[aerogpu_cmd_stream_header]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  ...
```

### Command stream writer

Command serialization uses the shared UMD implementation:

- `../common/aerogpu_cmd_stream_writer.h`

This provides both a `std::vector`-backed stream (portable bring-up/tests) and a span/DMA-backed stream (`{uint8_t* buf, size_t capacity}`) suitable for writing directly into a WDDM runtime-provided command buffer.

All append helpers return `nullptr` (and set `CmdStreamError`) on failure (for example: bounded DMA buffer full, or invalid payload arguments). Callers targeting a WDDM DMA buffer are expected to split/flush submissions and retry when out of space.

### Shared surface note

DXGI/D3D10/11 shared resource interop is implemented in the **Win7/WDDM 1.1 WDK builds** of this UMD:

- The protocol supports cross-process sharing via `AEROGPU_CMD_EXPORT_SHARED_SURFACE` /
  `AEROGPU_CMD_IMPORT_SHARED_SURFACE` and a stable cross-process `share_token` carried in preserved
  WDDM allocation private driver data (`aerogpu_wddm_alloc_priv.share_token` in
  `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
- Creating a shareable resource (for example: `D3D11_RESOURCE_MISC_SHARED`) causes the UMD to emit
  `AEROGPU_CMD_EXPORT_SHARED_SURFACE` exactly once after allocation, using the stable `share_token`
  returned in preserved WDDM allocation private driver data.
- Opening a shared resource (cross-process `OpenSharedResource`) causes the UMD to parse the preserved
  allocation private driver data and emit `AEROGPU_CMD_IMPORT_SHARED_SURFACE` using the same
  `share_token`.
- When opening shared resources created by **D3D9Ex** (legacy v1 private-data blobs),
  the D3D10/11 UMD falls back to the `reserved0` surface descriptor encoding
  (`AEROGPU_WDDM_ALLOC_PRIV_DESC_*` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`) to recover
  `width/height/format` and map the D3D9 format to a compatible `DXGI_FORMAT`.

On Win7/WDDM 1.1, `share_token` must be stable across guest processes. AeroGPU does
**not** use the numeric value of the user-mode shared `HANDLE` as `share_token`:
for real NT handles the numeric value is process-local (commonly different after
`DuplicateHandle`), and even token-style shared handles must not be treated as a
stable protocol key (and should not be passed to `CloseHandle`).

Canonical contract: on Win7/WDDM 1.1, the Win7 KMD generates a stable non-zero
`share_token` and persists it in the preserved WDDM allocation private driver data blob
(`aerogpu_wddm_alloc_priv.share_token` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
dxgkrnl returns the same bytes on cross-process opens, so both processes observe the
same `share_token`.

The preserved WDDM allocation private-data blob (`drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`)
is also used to persist a stable `alloc_id` across CreateAllocation/OpenAllocation so the
KMD can build the per-submit allocation table for guest-backed resources.

For shared allocations, `alloc_id` must avoid collisions across guest processes and must stay in the UMD-owned range (`alloc_id <= 0x7fffffff`, non-zero).

Canonical contract and rationale: `docs/graphics/win7-shared-surfaces-share-token.md`.

Win7 validation/regression tests:

- `drivers/aerogpu/tests/win7/d3d11_shared_surface_ipc/`
- `drivers/aerogpu/tests/win7/d3d10_shared_surface_ipc/`
- `drivers/aerogpu/tests/win7/d3d10_1_shared_surface_ipc/`

## Build

This code is intended to be built as a **DLL UMD** for Windows 7 SP1.

### Optional tracing (resource creation / swapchain bring-up)

For Win7 bring-up it is often useful to log the runtime's `CreateResource` inputs
for DXGI swapchain backbuffers. The UMD supports a lightweight trace flag:

* `AEROGPU_UMD_TRACE_RESOURCES`

When enabled, the UMD prints `CreateResource`, `RotateResourceIdentities`, and
`Present` details via the standard UMD logging helper (`AEROGPU_D3D10_11_LOG`),
tagged with `trace_resources:`.

The trace hooks are implemented in both the WDK-backed Win7 DDIs and the
repo-local ABI subset build so the same flag can be used regardless of header
source (`/p:AeroGpuUseWdkHeaders=1` vs `0`).

See `docs/graphics/win7-dxgi-swapchain-backbuffer.md` for the recommended probe
app and log interpretation workflow.

### Optional tracing (instanced draws)

When validating instancing support (for example `DrawInstanced` /
`DrawIndexedInstanced`), it can be useful to log the instanced draw parameters
directly from the UMD so you can confirm `instanceCount > 1` in DebugView/WinDbg
without needing to dump and decode the full cmd buffer.

* `AEROGPU_UMD_TRACE_DRAWS`

When enabled, the WDK-backed D3D10 UMD logs `DrawInstanced` /
`DrawIndexedInstanced` calls via `AEROGPU_D3D10_11_LOG`, tagged with
`trace_draws:`.

Build files provided:

- `aerogpu_d3d10_11.sln`
- `aerogpu_d3d10_11.vcxproj` (Win32 + x64)

The project is configured to output binaries that match the Win7 packaging INF:

- Win32: `aerogpu_d3d10.dll`
- x64: `aerogpu_d3d10_x64.dll`

### Exported entrypoints

The Win7 D3D10/D3D11 runtimes load the UMD and look up these exports by name:

- `OpenAdapter10`
- `OpenAdapter10_2`
- `OpenAdapter11`

On Win32, `__stdcall` would normally decorate the symbol names (for example,
`_OpenAdapter10@4`). The build uses module-definition files to ensure the DLL
exports the undecorated names expected by the runtimes:

- `aerogpu_d3d10_x86.def`
- `aerogpu_d3d10_x64.def`

For cross-checking header drift and x86 stdcall export decoration against the
real Win7-era WDK header set, see the tooling-only probe:

- `tools/wdk_abi_probe/`

## Win7 WDK ABI verification (recommended)

The Win7 D3D10/D3D11 runtimes load the UMD by **ABI contract**: exported entrypoint
names/calling conventions, and the exact layout of the D3D10DDI/D3D11DDI structs
passed across the boundary (notably the adapter/device/context function tables).

To make ABI drift obvious *before* debugging a Win7 loader crash, the repo includes:

- A standalone **WDK ABI probe** tool:
  - `tools/wdk_abi_probe/`
- Optional **compile-time ABI asserts** wired into WDK builds:
  - `src/aerogpu_d3d10_11_wdk_abi_asserts.h` (inert unless `AEROGPU_UMD_USE_WDK_HEADERS=1`)

The checked-in expected ABI snapshot lives in:

- `src/aerogpu_d3d10_11_wdk_abi_expected.h`

The Visual Studio project enables strict ABI enforcement automatically for WDK-header builds by defining:

- `AEROGPU_D3D10_11_WDK_ABI_ENFORCE_EXPECTED` (set when `/p:AeroGpuUseWdkHeaders=1`).

If you intentionally update the WDK/toolchain and ABI asserts start failing, regenerate the expected header from probe output using:

- `tools/wdk_abi_probe/gen_expected_header.py` (see `tools/wdk_abi_probe/README.md`).

#### Quick validation

From a Visual Studio Developer Command Prompt, inspect the DLL exports:

```cmd
dumpbin /exports aerogpu_d3d10.dll
dumpbin /exports aerogpu_d3d10_x64.dll
```

Verify the output contains the **undecorated** entrypoints:

- `OpenAdapter10`
- `OpenAdapter10_2`
- `OpenAdapter11`

On **Win32** builds, also confirm the raw stdcall-decorated names are present:

- `_OpenAdapter10@4`
- `_OpenAdapter10_2@4`
- `_OpenAdapter11@4`

Fast CI guardrail (no WDK required):

- `scripts/ci/check-aerogpu-d3d10-def-stdcall.py` validates that
  `aerogpu_d3d10_x86.def` stays in sync with the checked-in expected ABI stack
  byte counts in `src/aerogpu_d3d10_11_wdk_abi_expected.h`.

Recommended build entrypoint (MSBuild/WDK10):

```cmd
cd \path\to\repo
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
```

CI builds the same solution (and stages outputs under `out/drivers/aerogpu/`) via `ci/build-drivers.ps1`.

Optional: `drivers\aerogpu\build\build_all.cmd` is a convenience wrapper around MSBuild/WDK10 that stages outputs under `drivers\aerogpu\build\out\win7\...`.

The real Win7 UMD must be compiled against the official D3D10/11 user-mode DDI
headers from the **Windows Driver Kit (WDK)** (Windows Kits):

- `d3d10umddi.h`
- `d3d10_1umddi.h`
- `d3d11umddi.h`
- `d3dumddi.h`

On a typical modern WDK install, `d3d11umddi.h` is under:

- `C:\Program Files (x86)\Windows Kits\10\Include\<ver>\um\`
- `C:\Program Files (x86)\Windows Kits\10\Include\<ver>\shared\`

The Visual Studio project enables WDK headers by defining
`AEROGPU_UMD_USE_WDK_HEADERS=1` when `/p:AeroGpuUseWdkHeaders=1` (the default for
the UMD project, and what `drivers\aerogpu\build\build_all.cmd` passes when
staging Win7 binaries).

For a repo-only/self-contained build (no UMDDI headers installed), pass
`/p:AeroGpuUseWdkHeaders=0` to compile against the repo’s minimal compat ABI.
This mode is **not** expected to be ABI-compatible with the real Win7 D3D11
runtime; use it only for local development/bring-up.

Optional: if you have a WinDDK-style root (Win7-era `inc\\{api,ddk}` layout), set
`/p:AeroGpuWdkRoot="C:\WinDDK\7600.16385.1"` (or `%WINDDK%`/`%WDK_ROOT%`/`%WDKROOT%`)
to add `$(AeroGpuWdkRoot)\inc\{api,ddk}` to the include path and validate the
expected headers exist. If `AeroGpuWdkRoot` is unset, the build falls back to
the toolchain's standard include paths (common for Windows Kits 10+ installs).

## Install / Register (INF)

On Windows 7, the D3D10/D3D11 runtimes load the driver’s UMD based on registry values written by the display driver INF:

- `UserModeDriverName` (`REG_SZ`): native-bitness D3D10/11 UMD filename (include `.dll`)
- `UserModeDriverNameWow` (`REG_SZ`, x64 only): 32-bit D3D10/11 UMD filename for WOW64 apps

In the Win7 packaging INF (`drivers/aerogpu/packaging/win7/aerogpu_dx11.inf`), this UMD is registered as:

```inf
[AeroGPU_Device_AddReg_x86]
HKR,,UserModeDriverName,%REG_SZ%,"aerogpu_d3d10.dll"

[AeroGPU_Device_AddReg_amd64]
HKR,,UserModeDriverName,%REG_SZ%,"aerogpu_d3d10_x64.dll"
HKR,,UserModeDriverNameWow,%REG_SZ%,"aerogpu_d3d10.dll"
```

Then ensure the DLLs are copied into the correct system directories during installation:

- x86 Windows: `System32\aerogpu_d3d10.dll`
- x64 Windows:
  - `System32\aerogpu_d3d10_x64.dll` (64-bit)
  - `SysWOW64\aerogpu_d3d10.dll` (32-bit)

After installation, reboot and confirm adapter open calls in DebugView (`OutputDebugString`), including the resolved DLL path:

- `aerogpu-d3d10_11: module_path=...`
- `aerogpu-d3d10_11: OpenAdapter11 ...`

## DDI call logging (Win7 bring-up)

For early bring-up the UMD can emit a lightweight, grep-friendly trace of runtime → UMD calls.

- Output format: **one line per call**, prefixed with `AEROGPU_D3D11DDI:`
- Sink: `OutputDebugStringA` (and optionally an on-disk log file)

Enable it by setting these environment variables **before launching the app**:

```cmd
set AEROGPU_D3D10_11_LOG=1
rem Optional: also append to a file
set AEROGPU_D3D10_11_LOG_FILE=C:\aerogpu_d3d10_11_umd.log
rem Optional: trace MIC/Low-IL labeling of shared counter mappings (GlobalHandleCounter)
set AEROGPU_LOG_MIC=1
```

Collect the output using one of:

- **DebugView** (Sysinternals): run DebugView and enable *Capture Win32*.
- **WinDbg**: attach to the process and watch the debug output stream.
