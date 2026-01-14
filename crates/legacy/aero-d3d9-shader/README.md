# aero-d3d9-shader (legacy)

This crate contains an older **Direct3D 9 SM2/SM3 token-stream parser + debug disassembler**.

It is **not used by Aero's runtime D3D9 shader translation pipeline**. The canonical implementation
moving forward lives in:

- `crates/aero-d3d9/src/sm3`

The crate is kept around as a reference/debugging aid, but is excluded from the workspace to avoid
adding maintenance burden to workspace-wide builds.

## Building/tests

Because it is excluded from the workspace, build it directly:

```sh
cargo test --manifest-path crates/legacy/aero-d3d9-shader/Cargo.toml
```

Note: Cargo may generate a local `crates/legacy/aero-d3d9-shader/Cargo.lock` when building this crate
standalone.
