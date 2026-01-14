# D3D9 SM2/SM3 shader translation status (aero-d3d9)

This doc is a small “don’t duplicate work” scratchpad for the D3D9 Shader Model 2/3
translator (`crates/aero-d3d9/src/sm3/`).

It tracks task-level status for shader bytecode → IR → WGSL lowering work.

For the broader “scratchpad task ID → implementation/test” audit, see
[`task-489-sm3-dxbc-sharedsurface-audit.md`](./task-489-sm3-dxbc-sharedsurface-audit.md).

## Task status

### Task 401 — Texture sampling lowering (`texld`/`texldp`/`texldd`/`texldl`)

**Status:** ✅ Done

**What:** Texture sampling lowering via `IrOp::TexSample`, plus texture/sampler binding emission and
bind layout population (`bind_group_layout.{sampler_group,sampler_bindings,sampler_texture_types}`).

Sampler texture types come from `dcl_* s#` when present; when absent, samplers default to `Texture2D`
(and this default is recorded in `bind_group_layout.sampler_texture_types`).

**Where:**
- `crates/aero-d3d9/src/sm3/wgsl.rs`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs` (core texld/texldp/texldd/texldl + sampler `dcl_*` texture-type coverage)
- `crates/aero-d3d9/tests/sm3_wgsl_tex.rs` (additional sampler-type coverage)

**Binding contract (AeroGPU + translators):**
- `@group(0)` — constants buffer shared by VS/PS
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

## Remaining / known limitations (true delta)

- Sampler state mapping (filtering, address modes, LOD bias, etc.) is handled in the runtime pipeline setup,
  not in the SM2/SM3 WGSL generator. Comparison samplers / depth-compare sampling are not modeled here yet.
- The SM3 **software reference interpreter** (`crates/aero-d3d9/src/sm3/software.rs`) currently only models
  `Texture2D` sampling; it does not emulate 1D/3D/cube sampling.
