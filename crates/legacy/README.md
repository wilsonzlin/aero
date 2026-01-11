# Legacy / prototype crates

Crates under `crates/legacy/` are kept for reference only.

- They are **excluded** from the Cargo workspace.
- They may not compile on `main`.
- New code should not depend on them.

Canonical VM wiring lives in `crates/aero-machine` (`aero_machine::Machine`) (see `docs/vm-crate-map.md` and ADR 0008).
