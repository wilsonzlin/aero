# ADR 0005: Commit `Cargo.lock` for reproducible Rust builds

## Context

Aero is a production emulator with a large Rust workspace (WASM core, host tooling, tests, and utilities).
Without a committed lockfile, the exact dependency graph is **floating** and can change whenever crates.io
publishes new versions that still satisfy our semver requirements.

That makes it hard to:

- Reproduce CI failures locally.
- Bisect regressions introduced by dependency updates.
- Reason about security/advisories against a specific shipped dependency graph.

## Decision

We **commit `Cargo.lock`** for the root Rust workspace (and for standalone Rust workspaces/tools in this
repository) and treat lockfiles as part of the source of truth for builds.

Policy:

- The repository **must** contain an up-to-date lockfile for:
  - The root workspace: `./Cargo.lock`
  - Standalone nested workspaces/tools (each maintains its own `Cargo.lock` next to its `Cargo.toml`).
- CI runs Rust commands with `--locked` and fails if any command would modify a lockfile.
- CI verifies lockfile consistency via `cargo metadata --locked` (fails if `Cargo.toml` and `Cargo.lock` drift).
  - We intentionally avoid using `cargo generate-lockfile --locked` as a CI drift check: `cargo generate-lockfile`
    re-resolves to the latest compatible versions (like `cargo update`), so it can fail when crates are published
    between runs even if `Cargo.lock` is valid.

### Updating dependencies

Dependency updates happen via PRs:

1. **Automated:** Dependabot opens scheduled PRs for Cargo (weekly).
2. **Manual (when needed):**
   - Workspace-wide update: `cargo update -w`
   - Targeted update: `cargo update -p <crate>`
   - After changing `Cargo.toml`: run `cargo generate-lockfile`
   - For standalone tools: run the same commands from that tool directory (or pass `--manifest-path ...`).

PRs that change dependency requirements **must** include the corresponding `Cargo.lock` diff.

## Alternatives considered

1. **Do not commit `Cargo.lock`**
   - Pros: avoids lockfile churn in PRs.
   - Cons: builds are not reproducible; CI and local builds can resolve different dependency versions;
     regressions and advisories become harder to triage.

2. **Pin every dependency version exactly in `Cargo.toml`**
   - Pros: deterministic without lockfiles.
   - Cons: high maintenance burden; makes selective updates harder; doesn’t scale for a large workspace.

3. **Use `cargo vendor`**
   - Pros: maximum hermeticity.
   - Cons: large repository bloat; additional workflow complexity; not currently needed for Aero.

## Consequences

- Builds become reproducible: `cargo ... --locked` uses the committed dependency graph.
- CI fails fast when `Cargo.toml` and `Cargo.lock` drift.
- Contributors need to include lockfile updates in PRs that change Rust dependencies.
- The project must regularly accept dependency update PRs (automated or manual) to keep up with security fixes.
