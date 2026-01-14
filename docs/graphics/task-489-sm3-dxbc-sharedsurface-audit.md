# Task 489 audit: SM3 / DXBC / shared-surface tracking cleanup

This document maps legacy **scratchpad task IDs** (the ones referenced by Agent-3 planning) to the
current **in-tree implementations and tests**, to reduce duplicate work.

All file paths are repository-relative.

**Test-running notes (agent environments):**
- If you see Cargo stuck at `Blocking waiting for file lock on package cache`, retry with an isolated
  Cargo home to avoid shared registry lock contention:
  `AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh ...`
- On cold builds, some targets may need a longer timeout (e.g. `AERO_TIMEOUT=1200`).
- Some integration tests depend on `wgpu`/WebGPU availability and may auto-skip on headless systems.
  Set `AERO_REQUIRE_WEBGPU=1` to force these tests to fail instead of skipping.

---

## Task 40 — SM3 IR → WGSL generator + naga tests

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-d3d9/src/sm3/{decode.rs,ir.rs,ir_builder.rs,wgsl.rs,verify.rs}`

**Implementing commits (high-signal):**
- `81181fad` — `feat(aero-d3d9): add sm3 WGSL backend with stable varying locations`
- `62b870b1` — `feat(sm3): derive WGSL declarations from register usage`

**Tests (WGSL + naga validation):**
- `crates/aero-d3d9/tests/sm3_wgsl.rs` (many `naga::front::wgsl::parse_str` + validator assertions)
- `crates/aero-d3d9/tests/sm3_wgsl_math.rs`
- `crates/aero-d3d9/tests/sm3_loop_wgsl.rs`
- `crates/aero-d3d9/tests/sm3_fixtures_wgsl.rs` (naga validation for real `fxc`-produced DXBC fixtures)

**How to run:**
```bash
# CPU-only WGSL + naga validation coverage:
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_math --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_loop_wgsl --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_fixtures_wgsl --locked
```

---

## Task 49 — semantic-based VS input remap via `StandardLocationMap`

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-d3d9/src/vertex/location_map.rs` (`StandardLocationMap` + `AdaptiveLocationMap`)
- `crates/aero-d3d9/src/sm3/ir_builder.rs` (semantic-driven input remap / duplicate detection)
- `crates/aero-d3d9/src/sm3/wgsl.rs` (emits the remapped `@location(n)` interface)

**Notes:**
- The remap is implemented via `AdaptiveLocationMap`, which:
  - reserves the fixed legacy assignments from `StandardLocationMap` for common semantics, and
  - deterministically allocates any additional declared semantics (e.g. `TEXCOORD8`) to the lowest
    remaining free locations.

**Implementing commits (high-signal):**
- `be5d5b05` — `feat(aero-d3d9/sm3): remap vertex inputs to canonical WGSL locations`

**Tests:**
- `crates/aero-d3d9/tests/sm3_semantic_locations.rs`
- `crates/aero-d3d9/tests/sm3_wgsl_semantic_locations.rs`
- `crates/aero-d3d9/src/vertex/location_map.rs` (unit tests for `AdaptiveLocationMap` determinism + edge cases)
- `crates/aero-gpu/tests/aerogpu_d3d9_semantic_locations.rs`
- `tests/d3d9_vertex_input.rs` (integration coverage via `aero-d3d9` test harness wiring)

**How to run (focused):**
```bash
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_semantic_locations --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_semantic_locations --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --lib vertex::location_map --locked
bash ./scripts/safe-run.sh cargo test -p aero --test d3d9_vertex_input --locked
bash ./scripts/safe-run.sh cargo test -p aero-gpu --test aerogpu_d3d9_semantic_locations --locked
```

---

## Task 51 / 55 / 58 — DXBC parsing consolidation + `build_container` + RDEF/CTAB moved to `aero-dxbc`

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-dxbc/src/{dxbc.rs,lib.rs,rdef.rs,ctab.rs,signature.rs,test_utils.rs}`
- `crates/aero-d3d9/src/dxbc.rs` (uses `aero_dxbc::DxbcFile` for container parsing)
- (No separate wrapper crate) other crates should depend on `crates/aero-dxbc/` directly for DXBC parsing.

**Implementing commits (high-signal):**
- `2bb1bbca` — `refactor(d3d9): reuse aero-dxbc for shader bytecode extraction`
- `4447967e` — `feat(dxbc): add test utils container builder`
- `85f15d9f` — `feat(dxbc): unify RDEF/CTAB parsing in aero-dxbc`

**Tests:**
- `crates/aero-dxbc/src/tests.rs` (unit tests; includes `tests_{parse,rdef,rdef_ctab,signature,sm4}.rs`)
- `crates/aero-d3d9/tests/sm3_ir.rs` (uses `aero_dxbc::test_utils::build_container`)

**How to run:**
```bash
bash ./scripts/safe-run.sh cargo test -p aero-dxbc --locked
# `sm3_ir` uses `aero_dxbc::test_utils::build_container`.
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_ir --locked
```

---

## Task 60 / 102 — DXBC robust feature gating + robust parser moved to `aero-dxbc`

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-dxbc/src/lib.rs` (`#[cfg(feature = "robust")] pub mod robust;`)
- `crates/aero-dxbc/src/robust/*` (robust container parsing + reflection/disasm helpers)
- `crates/aero-d3d9/src/dxbc/robust.rs` (re-export shim)

**Implementing commits (high-signal):**
- `96c10295` — `refactor(dxbc): move robust DXBC parsing into aero-dxbc`

**Tests:**
- `crates/aero-dxbc/src/robust/container.rs` (robust parser unit tests; run with `--features robust`)
- `crates/aero-d3d9/tests/dxbc_parser.rs` (`#![cfg(feature = "dxbc-robust")]`)

**How to run:**
```bash
# Run the robust parser unit tests in `aero-dxbc` itself:
bash ./scripts/safe-run.sh cargo test -p aero-dxbc --features robust --locked

# Enables aero-dxbc's `robust` feature via aero-d3d9's `dxbc-robust` feature.
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --features dxbc-robust --test dxbc_parser --locked
```

---

## Task 62 / 66 / 69 — `SharedSurfaceTable` refactor across command processors + executors (D3D9/D3D11)

**Status:** ✅ Done

**See also:**
- [`docs/graphics/win7-shared-surfaces-share-token.md`](./win7-shared-surfaces-share-token.md) (the `share_token` vs user-mode `HANDLE` contract + collision/retirement policy; cross-process test pointers)

**Implementation (key files):**
- `crates/aero-gpu/src/shared_surface.rs` (single source of truth)
- Used by:
  - `crates/aero-gpu/src/command_processor.rs`
  - `crates/aero-gpu/src/command_processor_d3d9.rs`
  - `crates/aero-gpu/src/aerogpu_d3d9_executor.rs`
  - `crates/aero-gpu/src/acmd_executor.rs`
  - `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs` (via a thin wrapper)

**Implementing commits (high-signal):**
- `36c5e5f2` — `refactor(aero-gpu): use SharedSurfaceTable in command processor`
- `d37c607a` — `refactor: reuse SharedSurfaceTable in D3D9 command processor`
- `f75daac9` — `refactor(aero-gpu): use SharedSurfaceTable in D3D9 executor`
- `527ac6db8` — `refactor(shared-surface): reuse aero-gpu table in D3D11 executor`

**Tests:**
- `crates/aero-gpu/src/shared_surface.rs` (unit tests for token retirement/idempotency/etc)
- `crates/aero-gpu/src/tests/shared_surface.rs` (additional unit tests for alias/refcount/token rules)
- `crates/aero-gpu/tests/shared_surface_aliasing.rs`
- `crates/aero-gpu/tests/aerogpu_d3d9_shared_surface.rs`
- `crates/aero-gpu/tests/aerogpu_d3d9_cmd_stream_shared_surface.rs` (end-to-end cmd-stream coverage)
- `crates/aero-d3d11/tests/aerogpu_cmd_shared_surface.rs` (D3D11 executor integration coverage; may skip if wgpu/WebGPU is unavailable)

**Notes:**
- The D3D11 executor reuses the canonical `aero-gpu` shared-surface bookkeeping:
  - `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs` wraps `aero_gpu::SharedSurfaceTable`
    (thin error-message adaptation), so D3D9 + D3D11 share the same alias/refcount/token-retirement
    semantics.

**How to run (focused):**
```bash
# CPU-only unit tests for the shared-surface bookkeeping:
bash ./scripts/safe-run.sh cargo test -p aero-gpu --lib shared_surface::tests --locked
bash ./scripts/safe-run.sh cargo test -p aero-gpu --lib tests::shared_surface --locked

bash ./scripts/safe-run.sh cargo test -p aero-gpu --test shared_surface_aliasing --locked
bash ./scripts/safe-run.sh cargo test -p aero-gpu --test aerogpu_d3d9_shared_surface --locked
bash ./scripts/safe-run.sh cargo test -p aero-gpu --test aerogpu_d3d9_cmd_stream_shared_surface --locked

# D3D11 executor shared-surface behavior (wgpu/WebGPU; may skip unless AERO_REQUIRE_WEBGPU=1):
bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --test aerogpu_cmd_shared_surface --locked
```

---

## Task 85 / 87 / 88 / 92 / 93 / 94 — SM3 opcode + modifier + const support

Ops/features referenced by the scratchpad tasks:
`frc`, `cmp`, `mova`, `defi`, `defb`, source modifiers, `lrp`, `exp`, `log`, `pow`.

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-d3d9/src/sm3/{decode.rs,ir.rs,ir_builder.rs,wgsl.rs,software.rs,verify.rs}`

**Implementing commits (high-signal):**
- `d190c9a6` — `feat(sm3): support defi/defb consts in IR + WGSL`
- `9f4ff084` — `feat(aero-d3d9/sm3): support frc/cmp opcodes in decode/IR/WGSL`
- `6f0e9530` — `feat(sm3): add mova opcode and WGSL lowering for address regs`
- `570416e6` — `feat(aero-d3d9/sm3): support D3D9 src modifiers in decode+WGSL`
- `4c3f1e25` — `feat(sm3): support lrp and emit WGSL`
- `77dca861` — `feat(aero-d3d9/sm3): add exp/log/pow + WGSL lowering`

**Tests:**
- `crates/aero-d3d9/src/tests.rs`
  - `micro_ps2_src_and_result_modifiers_pixel_compare` (src modifiers + result modifiers)
  - `micro_ps3_lrp_pixel_compare`
  - `sm3_exp_log_pow_pixel_compare`
- `crates/aero-d3d9/tests/sm3_wgsl.rs` / `sm3_wgsl_math.rs` (naga-validated WGSL lowering)
- `crates/aero-d3d9/tests/sm3_wgsl_mova.rs` (mova + relative constant addressing)
- `crates/aero-d3d9/tests/sm3_wgsl_relative_const_defs.rs` (relative const indexing with many embedded `def` overrides)

**How to run:**
```bash
# CPU-only WGSL + naga validation coverage:
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_math --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_mova --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_relative_const_defs --locked
```

---

## Task 124 — D3D9 half-pixel center convention (`half_pixel_center`)

**Status:** ✅ Done

**What:** Optional emulation of D3D9’s classic “half-pixel offset” by nudging the final clip-space
vertex position by `(-1/viewport_width, +1/viewport_height) * w` in translated vertex shaders.
This is enabled via `WgslOptions::half_pixel_center` and is wired end-to-end through the D3D9
executor.

**Implementation (key files):**
- Translation (SM3-first path): `crates/aero-d3d9/src/shader_translate.rs`
  - `inject_half_pixel_center_sm3_vertex_wgsl` injects `@group(3) @binding(0)` `HalfPixel` uniform
    + clip-space adjustment.
- Translation (legacy fallback path): `crates/aero-d3d9/src/shader.rs`
  - `WgslOptions::half_pixel_center` emits the same uniform + adjustment for the legacy
    token-stream translator.
- Execution: `crates/aero-gpu/src/aerogpu_d3d9_executor.rs`
  - creates/binds the half-pixel bind group at `@group(3) @binding(0)`
  - updates the uniform on `AeroGpuCmd::SetViewport`.

**Tests:**
- `crates/aero-gpu/tests/aerogpu_d3d9_half_pixel_center.rs` (pixel-level rasterization shift)

**How to run:**
```bash
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-gpu --test aerogpu_d3d9_half_pixel_center --locked
```

---

## Task 125 / 400 — consistent VS↔PS varying location mapping + WGSL IO structs

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-d3d9/src/sm3/wgsl.rs` (emits `VsInput`/`VsOut`/`FsIn`/`FsOut` structs and stable `@location(n)` mapping)

**Implementing commits (high-signal):**
- `81181fad` — `feat(aero-d3d9): add sm3 WGSL backend with stable varying locations`
- `fdc5ee53` — `test(aero-d3d9/sm3): cover VS->PS varying @location mapping`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs::wgsl_vs_outputs_and_ps_inputs_use_consistent_locations`
- `crates/aero-d3d9/tests/sm3_wgsl_semantic_locations.rs::sm3_vs_output_and_ps_input_semantics_share_locations`

**How to run (focused):**
```bash
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_semantic_locations --locked
```

---

## Task 216 / 217 — `dp2` + `dsx`/`dsy` derivatives

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-d3d9/src/sm3/{decode.rs,ir_builder.rs,wgsl.rs,verify.rs}`

**Implementing commits (high-signal):**
- `571cfa54` — `feat(d3d9-sm3): add dp2 opcode end-to-end`
- `5067c94f` — `feat(d3d9-sm3): support dsx/dsy derivatives`
- `8660b7710` — `feat(d3d9): add dsx/dsy support to legacy shader translator` (fallback path)
- `4c9adf49c` — `feat(legacy): add dsx/dsy opcode support to aero-d3d9-shader parser` (reference disassembler)

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl_dp2.rs`
- `crates/aero-d3d9/tests/sm3_wgsl.rs`
  - `wgsl_dsx_dsy_derivatives_compile`
  - `wgsl_dsx_dsy_can_feed_texldd_gradients`
  - `wgsl_predicated_derivative_avoids_non_uniform_control_flow`
- `crates/aero-d3d9/tests/sm3_decode.rs`
  - `decode_rejects_dsx_in_vertex_shader`
  - `decode_rejects_dsy_in_vertex_shader`
- `crates/aero-d3d9/src/tests.rs::translate_entrypoint_legacy_fallback_supports_derivatives`

**How to run (focused):**
```bash
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_dp2 --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_decode --locked
```

---

## Task 401 / 402 — `TexSample` lowering/bindings + `texkill` semantics

**Status:** ✅ Done

**See also:** `docs/graphics/d3d9-sm2-sm3-shader-translation.md` (short “don’t duplicate work” status note for SM2/SM3 shader translation).

**Implemented:**
- WGSL lowering for `texld`/`texldp`/`texldb`/`texldd`/`texldl` (`textureSample*` variants) and bind group layout mapping for samplers/textures.
- `texkill` lowers to D3D9 semantics: `discard` when **any component** of the operand is `< 0`, and preserves predication nesting.
- Sampler declarations map texture types to WGSL texture bindings and coordinate dimensionality:
  - `dcl_1d` → `texture_1d<f32>` (`x`)
  - `dcl_2d` → `texture_2d<f32>` (`xy`)
  - `dcl_volume` → `texture_3d<f32>` (`xyz`)
  - `dcl_cube` → `texture_cube<f32>` (`xyz`)
  - If a sampler has no `dcl_*` declaration, it defaults to `Texture2D` and is recorded as such in `bind_group_layout.sampler_texture_types`.

**Implementation (key files):**
- `crates/aero-d3d9/src/sm3/wgsl.rs` (sampler bindings + `IrOp::TexSample` lowering + `Stmt::Discard`)
- `crates/aero-d3d9/src/sm3/ir_builder.rs` (decode → IR for tex ops)

**Implementing commits (high-signal):**
- `aa89e80b` — `feat(aero-d3d9/sm3): emit WGSL for TexSample ops`
- `57aa3f8c` — `fix(sm3): preserve texkill predication and D3D9 discard semantics`

**Tracking cleanup / additional coverage commits:**
- `04c80402` — `docs(d3d9-sm3): mark TexSample/texkill tasks done`
- `2f099e8dd` — `test(sm3): cover cube/3D sampler dcl in WGSL`
- `1dcec36ab` — `test(sm3): add 1D sampler dcl coverage`
- `0516665f7` — `test(sm3): cover non-2D texldp/texldd swizzles`
- `b6b6bec11` — `test(sm3): cover 1D texldp/texldd and clean up software matcher`
- `362261e8d` — `test(sm3): cover texldl swizzles for 1D/3D/cube samplers`
- `02e042470` — `fix(sm3): support texldb bias for 1D textures in WGSL`
- `dae0504ad` — `test(sm3): ensure predicated texldb avoids non-uniform control flow`
- `b10bb36f3` — `docs(graphics): mark SM3 TexSample/texkill tasks 401/402 done`
- `6617e2bc5` — `docs(graphics): cross-link SM3 shader translation task notes`
- `9f3c546f8` — `docs(graphics): link task-489 audit from SM3 translation notes`
- `5fb505938` — `docs(graphics): shorten SM3 shader translation status table`
- `e8523a8f9` — `test(sm3): assert default sampler texture types in bind layout`
- `de317d81a` — `test(sm3): cover translate_to_wgsl wrapper`
- `b0ccdf25e` — `docs(graphics): document default Texture2D sampler type`
- `9bb17163c` — `feat(d3d9): support cube textures in software shader interpreters`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs`
  - `sm3_translate_to_wgsl_wrapper_produces_bind_layout`
  - `wgsl_texld_emits_texture_sample`
  - `wgsl_texldp_emits_projective_divide`
  - `wgsl_texldb_emits_texture_sample_bias`
  - `wgsl_texldd_emits_texture_sample_grad`
  - `wgsl_vs_texld_emits_texture_sample_level`
  - `wgsl_texldl_emits_texture_sample_level_explicit_lod`
  - `wgsl_predicated_texld_avoids_non_uniform_control_flow`
  - `wgsl_predicated_texldb_avoids_non_uniform_control_flow`
  - `wgsl_predicated_texldb_1d_avoids_non_uniform_control_flow`
  - `wgsl_predicated_texldp_avoids_non_uniform_control_flow`
  - `wgsl_predicated_texldd_is_valid_with_non_uniform_predicate`
  - `wgsl_nonuniform_if_texld_avoids_invalid_control_flow`
  - `wgsl_nonuniform_if_else_texld_avoids_invalid_control_flow`
  - `wgsl_nonuniform_if_texldb_avoids_invalid_control_flow`
  - `wgsl_nonuniform_if_texldb_1d_avoids_invalid_control_flow`
  - `wgsl_nonuniform_if_predicated_texld_avoids_invalid_control_flow`
  - `wgsl_nonuniform_if_else_predicated_texld_avoids_invalid_control_flow`
  - `wgsl_dcl_1d_sampler_emits_texture_1d_and_x_coord`
  - `wgsl_dcl_1d_sampler_texldp_emits_projective_divide_x`
  - `wgsl_dcl_1d_sampler_texldb_emits_texture_sample_grad_x_with_bias`
  - `wgsl_dcl_1d_sampler_texldd_emits_texture_sample_grad_x`
  - `wgsl_dcl_1d_sampler_texldl_emits_texture_sample_level_x_lod`
  - `wgsl_dcl_cube_sampler_emits_texture_cube_and_xyz_coords`
  - `wgsl_dcl_volume_sampler_emits_texture_3d_and_xyz_coords`
  - `wgsl_dcl_cube_sampler_texldp_emits_projective_divide_xyz`
  - `wgsl_dcl_cube_sampler_texldb_emits_texture_sample_bias_xyz`
  - `wgsl_dcl_volume_sampler_texldp_emits_projective_divide_xyz`
  - `wgsl_dcl_cube_sampler_texldd_emits_texture_sample_grad_xyz`
  - `wgsl_dcl_volume_sampler_texldd_emits_texture_sample_grad_xyz`
  - `wgsl_dcl_cube_sampler_texldl_emits_texture_sample_level_xyz_lod`
  - `wgsl_dcl_volume_sampler_texldb_emits_texture_sample_bias_xyz`
  - `wgsl_dcl_volume_sampler_texldl_emits_texture_sample_level_xyz_lod`
  - `wgsl_texkill_is_conditional`
  - `wgsl_predicated_texkill_is_nested_under_if`
- `crates/aero-d3d9/tests/sm3_wgsl_tex.rs`
  - `wgsl_ps3_texldp_is_valid`
  - `wgsl_ps3_texld_cube_is_valid`
  - `wgsl_ps3_texkill_discard_is_valid`
  - `wgsl_ps3_texld_cube_sampler_emits_texture_cube`
  - `wgsl_ps3_texld_3d_sampler_emits_texture_3d`

**How to run (focused):**
```bash
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_tex --locked
```

**Notes / follow-ups:**
- The SM3 WGSL backend supports sampler texture types 1D/2D/3D/cube.
- WGSL does not support `textureSampleBias` for `texture_1d`; `texldb` for 1D samplers is lowered via
  `textureSampleGrad` with `dpdx`/`dpdy` scaled by `exp2(bias)`.
- The SM3 software reference interpreter (`crates/aero-d3d9/src/sm3/software.rs`) supports `Texture2D` + `TextureCube`
  sampling, but does not yet emulate 1D/3D textures.
- The legacy token-stream translator in `crates/aero-d3d9/src/shader.rs` supports sampler texture type declarations for
  1D/2D/3D/cube (`dcl_1d` / `dcl_2d` / `dcl_volume` / `dcl_cube`) and emits the corresponding WGSL bindings
  (`texture_1d<f32>` / `texture_2d<f32>` / `texture_3d<f32>` / `texture_cube<f32>`) with the correct coordinate
  dimensionality (`x` / `xy` / `xyz`).
  - Evidence: `crates/aero-d3d9/src/tests.rs::legacy_translator_emits_texture_1d_and_x_coords` and
    `crates/aero-d3d9/src/tests.rs::legacy_translator_emits_texture_3d_and_xyz_coords`.
- Note: the AeroGPU command protocol currently only creates 2D textures (`CREATE_TEXTURE2D`), so it cannot yet supply
  real guest-backed 1D/3D textures.
  - The SM3 WGSL backend can lower 1D/3D sampler declarations (used heavily in `aero-d3d9` naga-validation tests).
  - However, the D3D9 translation entrypoint (`translate_d3d9_shader_to_wgsl`) currently rejects shaders that *sample*
    from 1D/3D textures; unused `dcl_1d` / `dcl_volume` declarations are accepted. (See
    `docs/graphics/d3d9-sm2-sm3-shader-translation.md` for the current contract.)
- The WGSL generator does not attempt to model sampler *state* (filtering/address modes/LOD bias/etc.) directly;
  those are handled in runtime pipeline setup. Depth-compare sampling is also not modeled in the SM3 WGSL generator.
  (This is tracked in `docs/graphics/d3d9-sm2-sm3-shader-translation.md`.)

---

## Task 439 — SM3 PS MISCTYPE builtins (vPos/vFace)

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-d3d9/src/sm3/wgsl.rs` (builtin emission + local mapping for `misc0`/`misc1`)

**Implementing commits (high-signal):**
- `0b946c43c` — `feat(d3d9): Add WGSL support for SM3 vPos/vFace MISCTYPE`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl.rs`
  - `wgsl_ps3_vpos_misctype_builtin_compiles`
  - `wgsl_ps3_vface_misctype_builtin_compiles`

**How to run (focused):**
```bash
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl --locked
```

---

## Task 468 — SM3 PS oDepth / frag_depth

**Status:** ✅ Done

**Implementation (key files):**
- `crates/aero-d3d9/src/sm3/wgsl.rs` (maps `DepthOut` / `oDepth` to `@builtin(frag_depth)` and assigns from `oDepth.x`)

**Implementing commits (high-signal):**
- `a360c250a` — `feat(aero-d3d9): map SM2/SM3 oDepth to WGSL frag_depth`

**Tests:**
- `crates/aero-d3d9/tests/sm3_wgsl_depth_out.rs`
  - `wgsl_ps30_writes_odepth_emits_frag_depth`

**How to run (focused):**
```bash
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl_depth_out --locked
```
