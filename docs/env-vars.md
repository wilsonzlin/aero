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
| `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION` | When true, Aero will **not request GPU texture compression features** (BC/ETC2/ASTC) even if the adapter supports them. This forces Aero to treat texture compression support as unavailable (enabling CPU decompression fallbacks where implemented). Accepts `1/0/true/false/yes/no/on/off`. | `0` | wgpu device feature negotiation (`crates/aero-webgpu`, `crates/aero-gpu`, `crates/aero-d3d9`, etc.) and env normalization (`scripts/env/resolve.mjs`) | `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1 cargo test -p aero-webgpu`, `eval "$(node scripts/env/resolve.mjs --format bash --disable-wgpu-texture-compression)"` |
| `AERO_ALLOW_UNSUPPORTED_NODE` | If set to `1/true/yes/on`, `scripts/check-node-version.mjs` will **warn but exit 0** when the current Node.js version does not match the repo's pinned `.nvmrc`. Intended for constrained environments where you cannot install the pinned Node version. | *(unset)* | `scripts/check-node-version.mjs` (and therefore anything that calls it, like `cargo xtask`/`just`) | `AERO_ALLOW_UNSUPPORTED_NODE=1 cargo xtask test-all --skip-e2e` |
| `AERO_ISOLATE_CARGO_HOME` | If set to `1/true/yes/on`, agent helper scripts will override `CARGO_HOME` to a **repo-local** `./.cargo-home` to avoid global Cargo registry lock contention on shared hosts. | *(unset)* | `scripts/agent-env.sh`, `scripts/safe-run.sh`, Node test harnesses that spawn `cargo` (`tools/rust_l2_proxy.js`) | `AERO_ISOLATE_CARGO_HOME=1 source ./scripts/agent-env.sh` / `AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh cargo build --locked` |
| `AERO_DISABLE_RUSTC_WRAPPER` | If set to `1/true/yes/on`, agent helper scripts will force-disable rustc wrappers (`sccache`, etc) by clearing wrapper env vars before running Cargo commands. Useful if your environment injects a broken wrapper. | *(unset)* | `scripts/agent-env.sh`, `scripts/safe-run.sh` | `AERO_DISABLE_RUSTC_WRAPPER=1 source ./scripts/agent-env.sh` / `AERO_DISABLE_RUSTC_WRAPPER=1 bash ./scripts/safe-run.sh cargo test --locked` |
| `AERO_CARGO_BUILD_JOBS` | Positive integer. If set, agent helper scripts will use this for Cargo parallelism (`CARGO_BUILD_JOBS`). Useful for tuning parallelism in agent sandboxes where high parallelism can cause `rustc` thread-spawn failures (e.g. `failed to spawn helper thread (WouldBlock)`). | `1` (agent default) | `scripts/agent-env.sh`, `scripts/safe-run.sh` | `AERO_CARGO_BUILD_JOBS=2 source ./scripts/agent-env.sh` |
| `AERO_RUST_CODEGEN_UNITS` | Positive integer. If set, agent helper scripts will add `-C codegen-units=<n>` to `RUSTFLAGS` when running Cargo commands (unless `RUSTFLAGS` already contains a `codegen-units=` setting). This can reduce per-crate thread usage and improve reliability under tight thread/PID limits. Alias: `AERO_CODEGEN_UNITS`. | `min(CARGO_BUILD_JOBS, 4)` | `scripts/agent-env.sh`, `scripts/safe-run.sh` | `AERO_RUST_CODEGEN_UNITS=1 bash ./scripts/safe-run.sh cargo test --locked` / `AERO_CODEGEN_UNITS=1 source ./scripts/agent-env.sh` |
| `AERO_SAFE_RUN_RUSTC_RETRIES` | Positive integer (attempt count, including the first run). Controls how many times `scripts/safe-run.sh` will retry Cargo commands when it detects transient OS resource-limit failures during thread/process spawn (EAGAIN/WouldBlock), e.g. `failed to spawn helper thread (WouldBlock)` or `fork: retry: Resource temporarily unavailable`. Set to `1` to disable retries. | `3` | `scripts/safe-run.sh` | `AERO_SAFE_RUN_RUSTC_RETRIES=1 bash ./scripts/safe-run.sh cargo test --locked` |
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
