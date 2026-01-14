# aero-d3d9-shader (legacy)

This crate contains an older **Direct3D 9 SM2/SM3 token-stream parser + debug disassembler**.

It is **not used by Aero's runtime D3D9 shader translation pipeline**. The canonical implementation
moving forward lives in:

- `crates/aero-d3d9/src/sm3`

The crate is kept around as a reference/debugging aid.

## Building/tests

This crate is a **workspace member** so it can be exercised by unit tests:

```sh
cargo test -p aero-d3d9-shader --locked
```

When building it standalone via `--manifest-path`, Cargo may generate a local
`crates/legacy/aero-d3d9-shader/Cargo.lock`; this file is ignored by default.
