# ADR 0007: Rust crate naming (workspace package names)

## Context

Cargo package names are allowed to contain hyphens (e.g. `aero-jit`), but Rust
crate identifiers in code always use underscores (e.g. `aero_jit`).

The workspace previously contained multiple crates whose **package names**
normalize to the same Rust crate identifier (for example `aero-jit` and
`aero_jit` both become `aero_jit` in code). This creates ambiguity, forces
dependency renames, and makes long-term maintenance brittle.

## Decision

### 1) Package naming convention

- **All new Rust packages use `kebab-case`** in `[package].name`.
- Underscore package names (e.g. `aero_cpu_core`) are not allowed for new crates.
- In Rust code, crates are still imported using the standard normalized name
  (hyphens become underscores), e.g. `aero-cpu-core` is imported as
  `aero_cpu_core`.

### 2) Crate directory naming under `crates/`

- Crate directories under `crates/` must be **`kebab-case`** (no underscores).
- The directory name should match the package name when practical.

### 3) Legacy crates

- When a rename is necessary to avoid a collision with an existing canonical
  crate, prefer adding a **descriptive suffix** (example: `-x86`) rather than
  reintroducing underscores.
- If a legacy crate is truly abandoned, it should be removed from the workspace
  (or merged) rather than kept around under a colliding name.

### 4) Enforcement

CI runs `scripts/ci/check-crate-name-collisions.py` to ensure the workspace does
not introduce any new package-name collisions after `-` → `_` normalization.

## Alternatives considered

1. **Allow collisions and require dependency renames**
   - Rejected: spreads avoidable complexity throughout the repo and makes it
     hard to reason about which crate is being used.
2. **Standardize on underscore package names**
   - Rejected: goes against common Cargo ecosystem conventions and would require
     widespread renames across existing `kebab-case` crates.
3. **No convention (status quo)**
   - Rejected: guaranteed to regress over time without tooling enforcement.

## Consequences

- Workspace crates have consistent, ecosystem-standard package names.
- Renaming existing crates requires updating workspace members and dependent
  `Cargo.toml` paths; some integration tests/examples may need import updates.
- CI fails fast if a future change reintroduces a crate-name collision.

