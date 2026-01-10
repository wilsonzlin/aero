# Testing (local + CI)

This document is the practical companion to [`12-testing-strategy.md`](./12-testing-strategy.md). It focuses on **how to run Aero’s test stack locally**, how that maps to CI, and common browser-specific failure modes.

> **Policy note (fixtures):** The repository must not include proprietary Windows images/ISOs, BIOS ROMs, or other copyrighted firmware blobs. Tests and CI should run using **open fixtures** (synthetic images, open-source OS images, generated data). See [`FIXTURES.md`](./FIXTURES.md) and [`13-legal-considerations.md`](./13-legal-considerations.md). CI also enforces this via `scripts/ci/check-repo-policy.sh`.

---

## Quick start: run the full test suite

### Unified runner (recommended)

From the repo root:

```bash
./scripts/test-all.sh
```

The unified runner executes (in order):

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
3. `cargo test --workspace --all-features`
4. `wasm-pack test --node` (in the WASM crate)
5. `npm run test:unit`
6. `npm run test:e2e`

By default it sets `AERO_REQUIRE_WEBGPU=0` (matching CI) unless you explicitly enable it.

Common options:

```bash
# Skip the slowest step
./scripts/test-all.sh --skip-e2e

# Require WebGPU for tests that gate on it
./scripts/test-all.sh --webgpu

# Select Playwright projects (repeatable)
./scripts/test-all.sh --pw-project chromium --pw-project firefox

# Forward additional Playwright CLI args (everything after --)
./scripts/test-all.sh --pw-project chromium -- --grep smoke
```

If your repo layout differs from the defaults, override directories:

- `AERO_NODE_DIR` / `--node-dir`: the directory containing `package.json`
- `AERO_WASM_CRATE_DIR` / `--wasm-crate-dir`: the crate directory containing the WASM `Cargo.toml`

### Manual (equivalent) commands

From the repo root:

```bash
# Rust format/lint/test (host)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features

# Rust → WASM tests (run from the WASM crate directory; see below)
# wasm-pack test --node

# TypeScript / JS unit tests
npm ci
npm run test:unit

# Playwright E2E
npx playwright install --with-deps
npm run test:e2e
```

Notes:

- The `wasm-pack` step is usually run from the specific crate that produces WASM (often under `crates/`).
- Playwright browser downloads are large; CI typically caches them, but locally you only need to install once.

---

## Rust unit tests (host)

Run all Rust tests in the workspace:

```bash
cargo test --workspace
```

Run tests for a single crate:

```bash
cargo test -p <crate-name>
```

Run a single test (by name filter):

```bash
cargo test -p <crate-name> <test_name_substring>
```

Useful flags:

```bash
# Show stdout/stderr for passing tests
cargo test -p <crate-name> -- --nocapture

# Run ignored tests (if any are marked #[ignore])
cargo test -p <crate-name> -- --ignored
```

---

## WASM tests (Rust compiled to WebAssembly)

For crates that use `wasm-bindgen-test`, run tests in a Node environment:

```bash
# From the WASM crate directory (where Cargo.toml for the WASM crate lives)
wasm-pack test --node
```

Notes:

- `scripts/test-all.sh` auto-detects the WASM crate via `cargo metadata` (first crate with a `cdylib` target).
  - Override with `AERO_WASM_CRATE_DIR` / `--wasm-crate-dir` if needed.

Common pitfalls:

- **Wrong directory:** `wasm-pack` operates on a *single crate*. Run it from the crate that builds to WASM.
- **Missing target:** ensure the WASM target is installed:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```
- **Node vs browser environment:** `--node` does **not** provide DOM APIs (`document`, `window`, etc.). Keep `--node` tests focused on pure logic/WASM exports. If a test needs browser APIs, it should use a browser runner (e.g. `wasm-pack test --headless --chrome`) or be covered by Playwright.
- **WASM threads:** if a test requires `SharedArrayBuffer` / WASM threads, Node support may differ from browsers. Prefer testing thread-dependent behavior in a real browser (Playwright) where COOP/COEP can be enforced.

---

## TypeScript unit tests

Install dependencies:

```bash
npm ci
```

Run unit tests:

```bash
npm run test:unit
```

Run with coverage (most runners accept `--coverage` via argument passthrough):

```bash
npm run test:unit -- --coverage
```

Typical output locations:

- Terminal summary (pass/fail)
- `coverage/` directory (HTML + LCOV), depending on the runner configuration

---

## Playwright E2E tests

Run headless E2E tests:

```bash
npm run test:e2e
```

Run a specific browser project:

```bash
npx playwright test --project=chromium
npx playwright test --project=firefox
npx playwright test --project=webkit
```

Open Playwright UI mode (interactive runner):

```bash
npm run test:e2e -- --ui
```

Update snapshots (for screenshot/visual regression tests):

```bash
npm run test:e2e -- --update-snapshots
```

Debugging tips:

```bash
# Run a single test file
npm run test:e2e -- path/to/test.spec.ts

# Keep the browser open on failure (Playwright convention)
PWDEBUG=1 npm run test:e2e
```

If E2E tests fail early with errors about `SharedArrayBuffer` or `crossOriginIsolated`, see the COOP/COEP section below.

---

## COOP/COEP + `crossOriginIsolated` (SharedArrayBuffer / WASM threads)

### Why this matters

Aero relies on **WASM threads** and shared memory for performance (e.g. CPU emulation in Web Workers with `Atomics`). Browsers only expose `SharedArrayBuffer` in a **cross-origin isolated** context, which requires COOP/COEP headers.

If your page is not cross-origin isolated:

- `window.crossOriginIsolated` will be `false`
- `SharedArrayBuffer` may be `undefined`
- `WebAssembly.Memory({ shared: true, ... })` will fail
- any thread-dependent code will fail or silently fall back to single-thread behavior (depending on implementation)

### Required headers

Your dev server / test server must send:

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp` (or `credentialless` if supported and appropriate)

### How to verify (DevTools)

In the browser console:

```js
crossOriginIsolated
typeof SharedArrayBuffer
```

Expected:

- `crossOriginIsolated === true`
- `typeof SharedArrayBuffer === "function"`

Chrome also shows cross-origin isolation status in **DevTools → Security**.

### Common causes of failure

- **Serving from `file://`**: COOP/COEP isolation requires a proper origin; open the app via a dev server (and usually a secure context). `http://localhost` is treated as secure, but arbitrary `http://` origins are not.
- **Missing headers in your server/proxy**: ensure the *final* server (including any reverse proxy) sets COOP/COEP headers on HTML and subresources as needed.
- **Blocked cross-origin subresources under COEP**: with `Cross-Origin-Embedder-Policy: require-corp`, the browser will block cross-origin scripts/fonts/images that do not explicitly opt in via CORS or `Cross-Origin-Resource-Policy` headers. Symptoms show up as red errors in the console/network panel.
  - Fix by self-hosting assets, adding proper CORS, or using resources that send the correct headers.

---

## WebGPU testing policy

### Why WebGPU tests are gated in CI

WebGPU availability varies across:

- runners (most CI VMs do not have stable GPU access)
- operating systems / driver stacks
- headless browser configurations

To keep CI reliable, tests that **require** WebGPU are typically **skipped** unless explicitly requested. Tests should either:

- run with a non-WebGPU fallback (e.g. WebGL2) in default CI, or
- be conditionally enabled only when WebGPU is available and required

In Playwright, WebGPU-required tests are tagged with `@webgpu` and isolated in a
dedicated Playwright project, `chromium-webgpu` (see `playwright.config.ts`).
Default projects (`chromium`, `firefox`, `webkit`) exclude `@webgpu` tests to
keep PR CI stable on runners where WebGPU is missing or unreliable.

### Forcing WebGPU-required tests

Set `AERO_REQUIRE_WEBGPU=1` to make WebGPU a hard requirement:

```bash
# E2E (Playwright, WebGPU-only project)
AERO_REQUIRE_WEBGPU=1 npm run test:webgpu

# Or run the project directly
AERO_REQUIRE_WEBGPU=1 npx playwright test --project=chromium-webgpu

# Unit tests that exercise WebGPU-dependent paths (if applicable)
AERO_REQUIRE_WEBGPU=1 npm run test:unit
```

Expected behavior when `AERO_REQUIRE_WEBGPU=1` is set:

- tests **fail** (rather than skip/fallback) if `navigator.gpu` is missing or cannot create a device
- CI jobs that do not provide WebGPU will fail, by design

### CI workflows

- `.github/workflows/ci.yml` (PR CI) sets `AERO_REQUIRE_WEBGPU=0` and runs the
  non-WebGPU Playwright projects.
- `.github/workflows/webgpu.yml` (schedule + workflow_dispatch) sets
  `AERO_REQUIRE_WEBGPU=1` and runs the `chromium-webgpu` project; it uploads
  Playwright reports/artifacts so WebGPU regressions are actionable.

---

## CI behavior (what runs where)

CI should be reproducible locally with the same top-level commands:

- Full stack (recommended): `./scripts/test-all.sh`
- Rust: `cargo test --workspace`
- WASM (Node): `wasm-pack test --node`
- TypeScript unit tests: `npm run test:unit` (often with coverage enabled)
- Browser E2E: `npm run test:e2e`

### Cross-browser Playwright policy (PR vs scheduled)

Playwright E2E coverage is split into two workflows:

- **PR CI (fast):** `.github/workflows/ci.yml` runs Playwright in **Chromium only**
  (`--project=chromium`) to keep pull request feedback fast.
- **Cross-browser E2E (high confidence):** `.github/workflows/e2e-matrix.yml` runs a
  matrix over **Chromium + Firefox + WebKit** on a nightly schedule and via
  `workflow_dispatch`. This workflow is intended to catch browser-specific
  regressions without blocking PR merges.

Environment variables commonly affect CI behavior:

- `AERO_REQUIRE_WEBGPU=1`: require WebGPU (see above)

When debugging a CI failure locally, prefer matching the CI environment as closely as possible:

- use `npm ci` (not `npm install`) for deterministic dependency resolution
- run Playwright in headless mode (default) unless you specifically need `--ui`

---

## Rust microbenchmarks (Criterion)

We use [Criterion.rs](https://github.com/bheisler/criterion.rs) to measure a
small set of emulator-critical hot paths with stable statistics.

### Run the emulator-critical microbenchmarks

```bash
# Full (slower, more stable)
cargo bench -p aero_cpu_core --bench emulator_critical -- --noplot
```

Criterion writes results to `target/criterion/`.

Note: Criterion does **not** respect `CARGO_TARGET_DIR` for its output directory.
In CI we move `target/criterion` into `target/bench-*/criterion` so the base/head
runs don't overwrite each other.

### CI / PR profile (fast)

In CI we run a shorter benchmark configuration to keep PR runtime low:

```bash
AERO_BENCH_PROFILE=ci cargo bench -p aero_cpu_core --bench emulator_critical -- --noplot
```

## Benchmark regression CI

The workflow `.github/workflows/bench.yml` runs these microbenchmarks and fails
on regressions:

- **pull_request**: benchmarks the PR base commit and the PR head commit (same
  runner), then compares results. The workflow fails if any benchmark slows down
  by more than **10%**.
- **schedule / workflow_dispatch**: runs the suite on `main`, compares against
  the previous successful `main` run artifact, and uploads the current results
  as the new baseline artifact (`criterion`).

Artifacts:

- `bench-pr` contains both PR base + head Criterion results plus the comparison report.
- `criterion` is the moving baseline artifact for `main` (only updated on successful runs).
- `criterion-run-<run_id>` is uploaded for every `main` run to aid debugging.

### Manual comparison

You can compare two Criterion output directories locally:

```bash
python3 scripts/bench_compare.py \
  --base path/to/base/criterion \
  --new path/to/new/criterion \
  --threshold 0.10
```
