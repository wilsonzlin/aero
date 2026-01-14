# D3D9 SM2/SM3 shader translation status (aero-d3d9)

This doc is a small “don’t duplicate work” scratchpad for the D3D9 Shader Model 2/3
translator (`crates/aero-d3d9/src/sm3/`).

It tracks task-level status for shader bytecode → IR → WGSL lowering work.

Translation output is cached:

- Per-session in-memory cache: `crates/aero-d3d9/src/shader_translate.rs` (`ShaderCache`)
- WASM-only persistent cache (IndexedDB/OPFS): `crates/aero-d3d9/src/runtime/shader_cache.rs` +
  `web/gpu-cache/persistent_cache.ts` (wired into `crates/aero-gpu/src/aerogpu_d3d9_executor.rs`)

For the broader “scratchpad task ID → implementation/test” audit, see
[`task-489-sm3-dxbc-sharedsurface-audit.md`](./task-489-sm3-dxbc-sharedsurface-audit.md).

## Task status

### Task 216 / 217 — `dp2` + `dsx`/`dsy` derivatives

**Status:** ✅ Done

**What:** Support `dsx`/`dsy` derivative ops (lowered to WGSL `dpdx`/`dpdy`) and the `dp2` opcode, including
predication-safe lowering for derivatives (avoid non-uniform control flow).

**Where:**
- `crates/aero-d3d9/src/sm3/{decode.rs,ir_builder.rs,verify.rs,wgsl.rs}`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs`
  - `wgsl_dsx_dsy_derivatives_compile`
  - `wgsl_dsx_dsy_can_feed_texldd_gradients`
  - `wgsl_predicated_derivative_avoids_non_uniform_control_flow`
- `crates/aero-d3d9/tests/sm3_wgsl_dp2.rs`

### Task 401 — Texture sampling lowering (`texld`/`texldp`/`texldb`/`texldd`/`texldl`)

**Status:** ✅ Done

**What:** Texture sampling lowering via `IrOp::TexSample`, plus texture/sampler binding emission and
bind layout population (`bind_group_layout.{sampler_group,sampler_bindings,sampler_texture_types}`).

Sampler texture types come from `dcl_* s#` when present; when absent, samplers default to `Texture2D`
(and this default is recorded in `bind_group_layout.sampler_texture_types`).

Supported texture types in the SM3 WGSL backend: 1D/2D/3D/cube, with coordinate dimensionality
`x`/`xy`/`xyz` (including for `texldp`/`texldb`/`texldd`/`texldl`).

Note: The AeroGPU D3D9 runtime currently only supports binding 2D + cube textures from the command
stream. The translation entrypoint will accept `dcl_1d` / `dcl_volume` declarations when the sampler
is unused, but rejects shaders that actually sample from 1D/3D textures (see
`validate_sampler_texture_types` in `crates/aero-d3d9/src/shader_translate.rs`).

Note: WGSL does not support `textureSampleBias` for `texture_1d`, so SM3 `texldb` with a 1D sampler is
lowered via `textureSampleGrad` with `dpdx`/`dpdy` scaled by `exp2(bias)`.

Note: `texld`/`texldp`/`texldb` use implicit derivatives in WGSL (`textureSample*`) and must execute in
uniform control flow. Predicated texture sampling lowers via unconditional sampling + `select(...)`
rather than `if (p0) { ... }` to satisfy naga uniformity validation.

**Where:**
- `crates/aero-d3d9/src/sm3/wgsl.rs`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs` (core texld/texldp/texldd/texldl + sampler `dcl_*` texture-type coverage + wgpu pipeline-layout compatibility check)
- `crates/aero-d3d9/tests/sm3_wgsl_tex.rs` (additional sampler-type coverage)

**Binding contract (AeroGPU + translators):**
- `@group(0)` — constants shared by VS/PS (packed for stable bindings across stages)
  - `@binding(0)` — float4 constants (`c#`)
  - `@binding(1)` — int4 constants (`i#`)
  - `@binding(2)` — bool constants (`b#`, stored as `vec4<u32>` per register)
- `@group(1)` — VS samplers/textures
- `@group(2)` — PS samplers/textures
- For sampler `sN`, bindings are `(2*N, 2*N+1)` for `(texture, sampler)`

### Task 402 — `texkill` semantics + predication

**Status:** ✅ Done

**What:** `texkill` lowering (`discard` if **any** component `< 0`) and correct predication behavior
(predicated `texkill` must be nested under an `if`).

**Where:**
- `crates/aero-d3d9/src/sm3/ir_builder.rs` (`Opcode::TexKill`)
- `crates/aero-d3d9/src/sm3/wgsl.rs` (`Stmt::Discard`)

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs`
  - `wgsl_texkill_is_conditional`
  - `wgsl_predicated_texkill_is_nested_under_if`

### Task 439 — PS MISCTYPE (vPos/vFace) builtins

**Status:** ✅ Done

**What:** Pixel shader `MISCTYPE` builtins:

- `misc0` (vPos) maps to WGSL `@builtin(position)` (in `FsIn.frag_pos`) and is exposed to the shader
  body as `misc0: vec4<f32>`.
- `misc1` (vFace) maps to WGSL `@builtin(front_facing)` and is exposed to the shader body as a
  D3D-style `misc1: vec4<f32>` with `face` = `+1` or `-1`.

**Where:**
- `crates/aero-d3d9/src/sm3/wgsl.rs` (misc input tracking + builtin emission)

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs`
  - `wgsl_ps3_vpos_misctype_builtin_compiles`
  - `wgsl_ps3_vface_misctype_builtin_compiles`

### Task 468 — PS depth output (oDepth)

**Status:** ✅ Done

**What:** Support pixel shader depth output by lowering D3D9 `oDepth` / `RegFile::DepthOut` to WGSL
`@builtin(frag_depth)` and assigning from `oDepth.x`.

**Where:**
- `crates/aero-d3d9/src/sm3/wgsl.rs`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl_depth_out.rs`
  - `wgsl_ps30_writes_odepth_emits_frag_depth`

### Task 124 — D3D9 half-pixel center convention (`half_pixel_center`)

**Status:** ✅ Done

**What:** Optional emulation of D3D9’s “half-pixel offset” by nudging the final clip-space vertex
position by `(-1/viewport_width, +1/viewport_height) * w` in translated vertex shaders.

**Where:**
- SM3-first translation path: `crates/aero-d3d9/src/shader_translate.rs` (`inject_half_pixel_center_sm3_vertex_wgsl`)
- Legacy fallback translator: `crates/aero-d3d9/src/shader.rs` (`WgslOptions::half_pixel_center`)
- Executor plumbing: `crates/aero-gpu/src/aerogpu_d3d9_executor.rs` (bind group(3) uniform updated on `SetViewport`)

**Test:**
- `crates/aero-gpu/tests/aerogpu_d3d9_half_pixel_center.rs`

## Remaining / known limitations (true delta)

- Sampler state mapping (filtering, address modes, LOD bias, etc.) is handled in the runtime pipeline setup,
  not in the SM2/SM3 WGSL generator. Comparison samplers / depth-compare sampling are not modeled here yet.
- The runtime command stream currently supports binding only 2D + cube textures. Shaders that sample from 1D/3D
  textures are rejected at translation time; unused `dcl_1d` / `dcl_volume` declarations are accepted.
