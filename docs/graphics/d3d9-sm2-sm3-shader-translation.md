# D3D9 SM2/SM3 shader translation status (aero-d3d9)

This doc is a small “don’t duplicate work” scratchpad for the D3D9 Shader Model 2/3
translator (`crates/aero-d3d9/src/sm3/`).

It tracks task-level status for shader bytecode → IR → WGSL lowering work.

## Task status

| Task | Status | What | Where | Tests |
|------|--------|------|-------|-------|
| 401 | ✅ DONE | Texture sampling lowering (`texld`/`texldp`/`texldd`/`texldl`) via `IrOp::TexSample`, plus texture/sampler binding emission and bind layout population (`bind_group_layout.{sampler_group,sampler_bindings,sampler_texture_types}`). Binding contract: group(0)=constants, group(1)=VS samplers, group(2)=PS samplers; for sampler `sN`, bindings are `(2*N, 2*N+1)` for `(texture, sampler)`. | `crates/aero-d3d9/src/sm3/wgsl.rs` | `crates/aero-d3d9/tests/sm3_wgsl.rs` (`wgsl_texld_emits_texture_sample`, `wgsl_texldp_emits_projective_divide`, `wgsl_texldd_emits_texture_sample_grad`, `wgsl_vs_texld_emits_texture_sample_level`) |
| 402 | ✅ DONE | `texkill` lowering (`discard` if **any** component `< 0`) and correct predication behavior (predicated `texkill` must be nested under an `if`). | `crates/aero-d3d9/src/sm3/ir_builder.rs` (`Opcode::TexKill`) and `crates/aero-d3d9/src/sm3/wgsl.rs` (`Stmt::Discard`) | `crates/aero-d3d9/tests/sm3_wgsl.rs` (`wgsl_texkill_is_conditional`, `wgsl_predicated_texkill_is_nested_under_if`) |

## Remaining / known limitations (true delta)

- Sampler state mapping (filtering, address modes, LOD bias, etc.) is handled in the runtime pipeline setup,
  not in the SM2/SM3 WGSL generator. Comparison samplers / depth-compare sampling are not modeled here yet.
