# ADR 0009: Rust toolchain policy (pinned stable + pinned nightly for threaded WASM)

## Context

Aero is primarily a Rust project, with a web frontend that builds multiple WASM variants.

Most development and CI can use stable Rust, but the **threaded/shared-memory WASM build**
requires `-Z build-std` so the standard library is rebuilt with `atomics` enabled. That
requires a nightly toolchain.

Historically we relied on the moving `stable` and `+nightly` channels. This has two problems:

1. **Reproducibility:** a build done today may not match (or even compile) next week.
2. **Fragility:** nightly changes can break the threaded WASM build unexpectedly, blocking
   development and CI.

## Decision

### Stable toolchain (default)

- We **pin stable Rust to an explicit release** in `rust-toolchain.toml` (`channel = "1.xx.y"`).
- CI and local development should use this pinned version for all stable builds.

### Nightly toolchain (threaded WASM only)

- We **pin the nightly toolchain to a specific date** (`nightly-YYYY-MM-DD`) for threaded
  WASM builds.
- The pinned nightly string lives in a single source of truth: `scripts/toolchains.json`
  (`rust.nightlyWasm`).
- All threaded WASM build entrypoints must use this pinned nightly toolchain:
  - `web/scripts/build_wasm.mjs` (threaded variant)
  - `just setup` (installs the pinned nightly + `rust-src`)
  - CI (threaded WASM smoke build)

### Update cadence / ownership

- Toolchain bumps happen via PR and must keep CI green.
- **Stable**: bump intentionally when we need a new stable feature, or on a regular cadence
  (e.g. monthly/quarterly).
- **Nightly (threaded WASM)**: bump only when necessary (e.g. to fix a regression, or when a
  new nightly becomes required by the build).
- Any contributor may propose a bump; reviewers/maintainers are responsible for ensuring the
  pinned versions are compatible across supported platforms.

## Alternatives considered

1. **Track stable (`channel = "stable"`)**
   - Pros: always up to date, fewer manual bumps.
   - Cons: CI and local builds can change underneath us; harder to bisect toolchain-related issues.

2. **Use floating nightly (`+nightly`) for threaded WASM**
   - Pros: simplest to explain.
   - Cons: high risk of random breakage; undermines reproducibility for a key build artifact.

3. **Pin nightly but hardcode it in each script**
   - Pros: avoids a new config file.
   - Cons: duplicated strings drift easily; harder to update correctly.

## Consequences

- Builds become **more reproducible** across time and across environments.
- When toolchains are updated, the change is explicit and reviewable.
- The project must occasionally perform **toolchain bump maintenance**, but CI will catch
  drift (or accidental reintroduction of floating `nightly`) early.

