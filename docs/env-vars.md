# Environment variables (canonical)

This repository uses environment variables to configure build/test/dev tooling and a few runtime components.
To reduce drift, **prefer the canonical `AERO_*` variables** listed here and avoid legacy aliases.

## Precedence rules

1. **CLI flags** (when available) override environment variables.
2. **Canonical env vars** (e.g. `AERO_NODE_DIR`) override legacy aliases.
3. **Legacy aliases** are accepted for one compatibility cycle and emit a warning to stderr.
4. If nothing is set, tooling uses a **repo-default auto-detected** value when possible.

## Validation / normalization helper

Many scripts share the same “where is the Node workspace / wasm crate?” questions. Use:

```bash
node scripts/env/resolve.mjs --format json
```

It validates and normalizes inputs (paths, booleans, URLs) and uses the same path
resolution logic as CI (`scripts/ci/detect-node-dir.mjs`, `scripts/ci/detect-wasm-crate.mjs`).

It also normalizes a few commonly-propagated boolean knobs (e.g. `AERO_REQUIRE_WEBGPU`,
`AERO_DISABLE_WGPU_TEXTURE_COMPRESSION`, `VITE_DISABLE_COOP_COEP`) to `0`/`1`.

It can also print shell assignments:

```bash
eval "$(node scripts/env/resolve.mjs --format bash --require-node-dir --require-wasm-crate-dir)"
```

## Canonical variables

| Variable | Meaning | Default | Consumed by | Examples |
| --- | --- | --- | --- | --- |
| `AERO_NODE_DIR` | Path (relative to repo root or absolute) to the **Node workspace entrypoint** directory containing `package.json`. In the npm-workspaces layout this should usually stay at `.` (repo root); use `npm -w <path> …` to run scripts in other workspaces. | Auto-detected: prefer `.` then `frontend/` then `web/` | `cargo xtask test-all`, GitHub Actions CI, `.github/actions/setup-playwright`, `justfile` | `AERO_NODE_DIR=.`, `AERO_NODE_DIR=web` |
| `AERO_WASM_CRATE_DIR` | Path (relative to repo root or absolute) to the **wasm-pack Rust crate** (must contain `Cargo.toml` and declare a `cdylib`). | Auto-detected via `scripts/ci/detect-wasm-crate.mjs` (prefers `crates/aero-wasm`; otherwise uses `cargo metadata` only when the `cdylib` crate is unambiguous; fails if ambiguous). | `cargo xtask test-all`, GitHub Actions CI | `AERO_WASM_CRATE_DIR=crates/aero-wasm` |
| `AERO_REQUIRE_WEBGPU` | When true, WebGPU-tagged browser tests **must fail** if WebGPU is unavailable (instead of skipping). Accepts `1/0/true/false/yes/no/on/off`. | `0` | `cargo xtask test-all`, Playwright specs (`tests/**`) | `AERO_REQUIRE_WEBGPU=1 npm run test:e2e` |
| `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION` | When true, Aero will **not request GPU texture compression features** (BC/ETC2/ASTC) even if the adapter supports them. This forces Aero to treat texture compression support as unavailable (enabling CPU decompression fallbacks where implemented). Note: Aero also avoids requesting these compression features on the **wgpu GL backend** by default due to correctness issues; this env var is an additional opt-out for other backends. Accepts `1/0/true/false/yes/no/on/off`. | `0` | wgpu device feature negotiation (`crates/aero-webgpu`, `crates/aero-gpu`, `crates/aero-d3d9`, etc.) and env normalization (`scripts/env/resolve.mjs`) | `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1 cargo test -p aero-webgpu --locked`, `eval "$(node scripts/env/resolve.mjs --format bash --disable-wgpu-texture-compression)"` |
| `AERO_ALLOW_UNSUPPORTED_NODE` | If set to `1/true/yes/on`, `scripts/check-node-version.mjs` will **warn but exit 0** when the current Node.js version does not match the repo's pinned `.nvmrc`. Intended for constrained environments where you cannot install the pinned Node version. Note: `scripts/agent-env.sh` may auto-enable this when it detects a Node major mismatch. | *(unset)* | `scripts/check-node-version.mjs` (and therefore anything that calls it, like `cargo xtask`/`just`) | `AERO_ALLOW_UNSUPPORTED_NODE=1 cargo xtask test-all --skip-e2e` |
| `AERO_ISOLATE_CARGO_HOME` | Controls Cargo home isolation for agent sandboxes. If set to `1/true/yes/on`, agent helper scripts override `CARGO_HOME` to a **repo-local** `./.cargo-home` to avoid global Cargo registry lock contention on shared hosts. If set to any other non-false value, it is treated as a path (supports `~/` expansion; non-absolute paths are relative to repo root) and used as `CARGO_HOME`. Note: `scripts/safe-run.sh` will also auto-use an existing `./.cargo-home` when `AERO_ISOLATE_CARGO_HOME` is unset and `CARGO_HOME` is unset/default. | *(unset)* | `scripts/agent-env.sh`, `scripts/safe-run.sh`, Node test harnesses that spawn `cargo` (`tools/rust_l2_proxy.js`) | `AERO_ISOLATE_CARGO_HOME=1 source ./scripts/agent-env.sh` / `AERO_ISOLATE_CARGO_HOME=/tmp/aero-cargo-home bash ./scripts/safe-run.sh cargo build --locked` |
| `AERO_DISABLE_RUSTC_WRAPPER` | If set to `1/true/yes/on`, agent helper scripts will **force-disable** rustc wrappers by exporting empty wrapper env vars (which overrides global Cargo config). When unset, helper scripts still clear **environment-based sccache** wrappers by default for reliability, but preserve other wrappers like `ccache`. | *(unset)* | `scripts/agent-env.sh`, `scripts/safe-run.sh` | `AERO_DISABLE_RUSTC_WRAPPER=1 source ./scripts/agent-env.sh` / `AERO_DISABLE_RUSTC_WRAPPER=1 bash ./scripts/safe-run.sh cargo test --locked` |
| `AERO_CARGO_BUILD_JOBS` | Positive integer. If set, agent helper scripts will use this for Cargo parallelism (`CARGO_BUILD_JOBS`). Useful for tuning parallelism in agent sandboxes where high parallelism can cause `rustc` OS-resource panics (e.g. `failed to spawn helper thread (WouldBlock)` or `called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }`). | `1` (agent default) | `scripts/agent-env.sh`, `scripts/safe-run.sh` | `AERO_CARGO_BUILD_JOBS=2 source ./scripts/agent-env.sh` |
| `RUSTC_WORKER_THREADS` | Positive integer. Controls rustc's internal worker pool size. Agent helper scripts default/sanitize it to `CARGO_BUILD_JOBS` for reliability under tight thread limits. | *(unset)* (agent default: `CARGO_BUILD_JOBS`) | rustc, `scripts/agent-env.sh`, `scripts/safe-run.sh` | `RUSTC_WORKER_THREADS=1 bash ./scripts/safe-run.sh cargo build --locked` |
| `RAYON_NUM_THREADS` | Positive integer. Controls Rayon global thread pool size (used by rustc and other crates). Agent helper scripts default/sanitize it to `CARGO_BUILD_JOBS` for reliability under tight thread limits. | *(unset)* (agent default: `CARGO_BUILD_JOBS`) | Rayon, `scripts/agent-env.sh`, `scripts/safe-run.sh` | `RAYON_NUM_THREADS=1 bash ./scripts/safe-run.sh cargo build --locked` |
| `RUST_TEST_THREADS` | Positive integer. Controls Rust's built-in test harness parallelism (libtest). Agent helper scripts default it to `CARGO_BUILD_JOBS` for reliability under tight thread limits. | *(unset)* (libtest default: `num_cpus`; agent default: `CARGO_BUILD_JOBS`) | libtest (`cargo test`), `scripts/agent-env.sh`, `scripts/safe-run.sh` | `RUST_TEST_THREADS=1 bash ./scripts/safe-run.sh cargo test --locked` |
| `AERO_RUST_CODEGEN_UNITS` | Optional positive integer. If set, agent helper scripts will add `-C codegen-units=<n>` to `RUSTFLAGS` when running Cargo commands (unless `RUSTFLAGS` already contains a `codegen-units=` setting). This can reduce per-crate thread usage and improve reliability under tight thread/PID limits. Alias: `AERO_CODEGEN_UNITS`. | *(unset)* | `scripts/agent-env.sh`, `scripts/safe-run.sh` | `AERO_RUST_CODEGEN_UNITS=1 bash ./scripts/safe-run.sh cargo test --locked` / `AERO_CODEGEN_UNITS=1 source ./scripts/agent-env.sh` |
| `AERO_SAFE_RUN_RUSTC_RETRIES` | Positive integer (attempt count, including the first run). Controls how many times `scripts/safe-run.sh` will retry Rust build/test commands (Cargo and common wrappers like `npm`/`wasm-pack`) when it detects transient OS resource-limit failures during thread/process spawn (EAGAIN/WouldBlock), e.g. `failed to spawn helper thread (WouldBlock)`, `called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }`, or `fork: retry: Resource temporarily unavailable`. Set to `1` to disable retries. | `3` | `scripts/safe-run.sh` | `AERO_SAFE_RUN_RUSTC_RETRIES=1 bash ./scripts/safe-run.sh cargo test --locked` |
| `AERO_TIMEOUT` | Timeout in seconds used by `scripts/safe-run.sh` (wraps commands via `with-timeout.sh`). | `600` | `scripts/safe-run.sh` | `AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test --locked` |
| `AERO_MEM_LIMIT` | Virtual address space (RLIMIT_AS) limit used by `scripts/safe-run.sh` (passed through to `run_limited.sh`). | `12G` | `scripts/safe-run.sh` | `AERO_MEM_LIMIT=32G bash ./scripts/safe-run.sh npm -w web run test:unit` |
| `AERO_NODE_TEST_MEM_LIMIT` | Fallback RLIMIT_AS limit used by `scripts/safe-run.sh` for `node --test ...` when `AERO_MEM_LIMIT` is unset (helps WASM-heavy Node test runs under RLIMIT_AS). | `32G` | `scripts/safe-run.sh` | `AERO_NODE_TEST_MEM_LIMIT=24G bash ./scripts/safe-run.sh node --test tests/*.test.js` |
| `AERO_BENCH_COMPARE_PROFILE` | Optional perf-threshold profile name for benchmark comparison reports (e.g. `pr-smoke`, `nightly`). | Auto-detected by `scripts/compare-benchmarks.sh` based on input layout | `scripts/compare-benchmarks.sh` | `AERO_BENCH_COMPARE_PROFILE=pr-smoke bash ./scripts/compare-benchmarks.sh` |
| `AERO_BENCH_THRESHOLDS_FILE` | Path (relative to repo root or absolute) to the benchmark regression thresholds JSON file. | `bench/perf_thresholds.json` | `scripts/compare-benchmarks.sh` | `AERO_BENCH_THRESHOLDS_FILE=bench/perf_thresholds.json bash ./scripts/compare-benchmarks.sh` |
| `AERO_BENCH_COMPARE_MARKDOWN_OUT` | Output path for the generated benchmark comparison markdown report. | `bench_reports/compare.md` | `scripts/compare-benchmarks.sh` | `AERO_BENCH_COMPARE_MARKDOWN_OUT=/tmp/compare.md bash ./scripts/compare-benchmarks.sh` |
| `AERO_BENCH_COMPARE_JSON_OUT` | Output path for the generated benchmark comparison JSON artifact. | `bench_reports/compare.json` | `scripts/compare-benchmarks.sh` | `AERO_BENCH_COMPARE_JSON_OUT=/tmp/compare.json bash ./scripts/compare-benchmarks.sh` |
| `AERO_QEMU` | Path to a QEMU binary for Rust QEMU boot tests (`qemu-system-i386` / `qemu-system-x86_64`). | Auto-detected (`qemu-system-i386`, else `qemu-system-x86_64`) | Rust QEMU harness (`tests/harness/mod.rs`) | `AERO_QEMU=/usr/bin/qemu-system-i386 cargo test -p emulator --test boot_sector --locked` |
| `AERO_ARTIFACT_DIR` | Output directory for failing test artifacts (e.g. screenshots/diffs). | `target/aero-test-artifacts` | Rust QEMU harness (`tests/harness/mod.rs`) | `AERO_ARTIFACT_DIR=/tmp/aero-artifacts cargo test -p emulator --test windows7_boot -- --ignored` |
| `AERO_REQUIRE_TEST_IMAGES` | When set (to any value), missing test fixture assets are treated as **hard errors** instead of skips. | *(unset)* | Rust QEMU harness (`tests/harness/mod.rs`) | `AERO_REQUIRE_TEST_IMAGES=1 cargo test -p emulator --test freedos_boot --locked` |
| `AERO_UPDATE_TRACE_FIXTURES` | When set (to any value), GPU trace fixture tests will overwrite/regenerate the committed fixture files instead of asserting stability. | *(unset)* | `crates/aero-gpu-trace` tests, `crates/aero-gpu-trace-replay` tests | `AERO_UPDATE_TRACE_FIXTURES=1 cargo test -p aero-gpu-trace --locked` |
| `AERO_WINDOWS7_IMAGE` | Path to a user-supplied Windows 7 disk image for the gated/ignored `windows7_boot` test. | `test-images/local/windows7.img` (gitignored; not provided by repo) | `tests/windows7_boot.rs`, `scripts/prepare-windows7.sh` | `AERO_WINDOWS7_IMAGE=/path/to/windows7.img cargo test -p emulator --test windows7_boot -- --ignored` |
| `AERO_WINDOWS7_GOLDEN` | Path to an expected “golden” Windows 7 framebuffer screenshot to compare against for the gated/ignored `windows7_boot` test. | `test-images/local/windows7_login.png` (gitignored) | `tests/windows7_boot.rs`, `scripts/prepare-windows7.sh` | `AERO_WINDOWS7_GOLDEN=/path/to/windows7_login.png cargo test -p emulator --test windows7_boot -- --ignored` |
| `AERO_PLAYWRIGHT_REUSE_SERVER` | When set to `1/true`, Playwright will reuse already-running harness/preview/CSP servers instead of starting new ones. Disabled on CI. | `0` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_REUSE_SERVER=1 npm run test:e2e` |
| `AERO_PLAYWRIGHT_EXPOSE_GC` | When set to `1`, Playwright will pass `--expose-gc` to Chromium (via `--js-flags=--expose-gc`) for tests that need manual GC. | `0` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_EXPOSE_GC=1 npm run test:e2e -- --project chromium` |
| `AERO_PLAYWRIGHT_DEV_PORT` | TCP port for the Playwright dev harness server (`npm run dev:harness`). | Auto-detected free port starting at `5173` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_DEV_PORT=5174 npm run test:e2e` |
| `AERO_PLAYWRIGHT_DEV_ORIGIN` | Origin (URL, must include explicit port) for the Playwright dev harness server. When set, it overrides `AERO_PLAYWRIGHT_DEV_PORT` and is exported back into the environment for tests that need the resolved origin. | Auto: `http://127.0.0.1:<devPort>` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_DEV_ORIGIN=http://127.0.0.1:5174 npm run test:e2e` |
| `AERO_PLAYWRIGHT_PREVIEW_PORT` | TCP port for the Playwright COI preview server (`npm run serve:coi:harness`). | Auto-detected free port starting at `4173` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_PREVIEW_PORT=4174 npm run test:e2e` |
| `AERO_PLAYWRIGHT_PREVIEW_ORIGIN` | Origin (URL, must include explicit port) for the Playwright COI preview server. When set, it overrides `AERO_PLAYWRIGHT_PREVIEW_PORT` and is exported back into the environment for tests that need the resolved origin. | Auto: `http://127.0.0.1:<previewPort>` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_PREVIEW_ORIGIN=http://127.0.0.1:4174 npm run test:e2e` |
| `AERO_PLAYWRIGHT_CSP_PORT` | TCP port for the Playwright CSP test server (`node server/poc-server.mjs`). | Auto-detected free port starting at `4180` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_CSP_PORT=4181 npm run test:e2e` |
| `AERO_PLAYWRIGHT_CSP_ORIGIN` | Origin (URL, must include explicit port) for the Playwright CSP test server. When set, it overrides `AERO_PLAYWRIGHT_CSP_PORT` and is exported back into the environment for tests that need the resolved origin. | Auto: `http://127.0.0.1:<cspPort>` | `playwright.config.ts`, `playwright.gpu.config.ts` | `AERO_PLAYWRIGHT_CSP_ORIGIN=http://127.0.0.1:4181 npm run test:e2e` |
| `VITE_DISABLE_COOP_COEP` | Disable COOP/COEP response headers on dev/preview servers (useful for validating the non-`SharedArrayBuffer` fallback). Accepts `1/0/true/false/yes/no/on/off`. | `0` | `vite.harness.config.ts`, `web/vite.config.ts`, `web/scripts/serve.cjs`, `server/poc-server.mjs` | `VITE_DISABLE_COOP_COEP=1 npm run dev` |

## Backend / proxy URL variables (used by networking tooling)

These are not required for the core build/test pipeline, but are common when running local networking relays.

| Variable | Meaning | Default | Consumed by | Examples |
| --- | --- | --- | --- | --- |
| `UDP_RELAY_BASE_URL` | Base URL of the UDP relay service (`proxy/webrtc-udp-relay`) used by `backend/aero-gateway` to return `udpRelay` connection metadata in `POST /session`. Accepts `http(s)://` or `ws(s)://`. Browser clients must normalize between HTTP and WebSocket schemes when calling relay endpoints (e.g. `fetch()` does not support `ws(s)://`). | *(unset)* | `backend/aero-gateway` | `UDP_RELAY_BASE_URL=https://relay.example.com`, `UDP_RELAY_BASE_URL=wss://relay.example.com` |
| `UDP_RELAY_AUTH_MODE` | Relay auth mode used by `backend/aero-gateway` when minting `udpRelay.token` (`none`, `api_key`, `jwt`). | `none` | `backend/aero-gateway` | `UDP_RELAY_AUTH_MODE=jwt` |
| `UDP_RELAY_API_KEY` | API key to return when `UDP_RELAY_AUTH_MODE=api_key` (intended for local/dev only). | *(unset)* | `backend/aero-gateway` | `UDP_RELAY_API_KEY=dev-key` |
| `UDP_RELAY_JWT_SECRET` | HS256 secret used when `UDP_RELAY_AUTH_MODE=jwt` (gateway mints short-lived JWTs for the relay). | *(unset)* | `backend/aero-gateway` | `UDP_RELAY_JWT_SECRET=...` |
| `UDP_RELAY_TOKEN_TTL_SECONDS` | Token lifetime in seconds for gateway-minted relay credentials. | `300` | `backend/aero-gateway` | `UDP_RELAY_TOKEN_TTL_SECONDS=300` |
| `UDP_RELAY_AUDIENCE` | Optional JWT `aud` claim when `UDP_RELAY_AUTH_MODE=jwt`. | *(unset)* | `backend/aero-gateway` | `UDP_RELAY_AUDIENCE=relay` |
| `UDP_RELAY_ISSUER` | Optional JWT `iss` claim when `UDP_RELAY_AUTH_MODE=jwt`. | *(unset)* | `backend/aero-gateway` | `UDP_RELAY_ISSUER=aero-gateway` |
| `AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL` | Public base URL for the WebRTC UDP relay (logging only). Must be a valid URL. | *(unset)* | `proxy/webrtc-udp-relay` | `AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL=https://relay.example.com` |
| `AERO_STUN_URLS` | Comma-separated STUN URLs (e.g. `stun:stun.l.google.com:19302`). | *(unset)* | `proxy/webrtc-udp-relay` | `AERO_STUN_URLS=stun:stun.l.google.com:19302` |
| `AERO_TURN_URLS` | Comma-separated TURN URLs. | *(unset)* | `proxy/webrtc-udp-relay` | `AERO_TURN_URLS=turn:turn.example.com:3478?transport=udp` |

## Deprecated aliases (will be removed)

These names are still accepted but emit warnings. Prefer the canonical variables above.

| Deprecated | Use instead |
| --- | --- |
| `AERO_WEB_DIR` | `AERO_NODE_DIR` |
| `WEB_DIR` | `AERO_NODE_DIR` |
| `AERO_WASM_DIR` | `AERO_WASM_CRATE_DIR` |
| `WASM_CRATE_DIR` | `AERO_WASM_CRATE_DIR` |
