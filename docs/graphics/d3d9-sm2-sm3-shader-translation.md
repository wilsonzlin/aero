# D3D9 SM2/SM3 shader translation status (aero-d3d9)

This doc is a small “don’t duplicate work” scratchpad for the D3D9 Shader Model 2/3
translator (`crates/aero-d3d9/src/sm3/`).

For the broader “scratchpad task ID → implementation/test” audit doc, see:
- `docs/graphics/task-489-sm3-dxbc-sharedsurface-audit.md`

## Task status

| Task | Status | What | Where | Tests |
|------|--------|------|-------|-------|
| 401 | ✅ DONE | Texture sampling lowering (`texld`/`texldp`/`texldd`/`texldl`) via `IrOp::TexSample`, plus sampler binding emission and bind layout population (`bind_group_layout.{sampler_group,sampler_bindings,sampler_texture_types}`). Bind contract: group(0)=constants, group(1)=VS samplers, group(2)=PS samplers; for sampler `sN`, bindings are `(2*N, 2*N+1)` for `(texture, sampler)`. | `crates/aero-d3d9/src/sm3/wgsl.rs` | `crates/aero-d3d9/tests/sm3_wgsl.rs` |
| 402 | ✅ DONE | `texkill` lowering (`discard` if any component `< 0`) and predication semantics (predicated `texkill` nests under an `if`). | `crates/aero-d3d9/src/sm3/{ir_builder.rs,wgsl.rs}` | `crates/aero-d3d9/tests/sm3_wgsl.rs` |

## Remaining / known limitations (true delta)

- Sampler state mapping (filtering, address modes, LOD bias, etc.) is handled in the runtime pipeline setup,
  not in the SM2/SM3 WGSL generator. Comparison samplers / depth-compare sampling are not modeled here yet.

