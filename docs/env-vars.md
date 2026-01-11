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

It validates and normalizes inputs (paths, booleans, URLs) and can print shell assignments:

```bash
eval "$(node scripts/env/resolve.mjs --format bash --require-node-dir --require-wasm-crate-dir)"
```

## Canonical variables

| Variable | Meaning | Default | Consumed by | Examples |
| --- | --- | --- | --- | --- |
| `AERO_NODE_DIR` | Path (relative to repo root or absolute) to the **Node workspace** directory containing `package.json`. | Auto-detected: prefer `.` then `frontend/` then `web/` | `scripts/test-all.sh`, GitHub Actions CI, `justfile` | `AERO_NODE_DIR=.`<br>`AERO_NODE_DIR=web` |
| `AERO_WASM_CRATE_DIR` | Path (relative to repo root or absolute) to the **wasm-pack Rust crate** (must contain `Cargo.toml`). | Auto-detected: prefer `crates/aero-wasm`, `crates/wasm`, `crates/aero-ipc`, `wasm/`, `rust/wasm/` (then `cargo metadata` fallback) | `scripts/test-all.sh`, GitHub Actions CI | `AERO_WASM_CRATE_DIR=crates/aero-wasm` |
| `AERO_REQUIRE_WEBGPU` | When true, WebGPU-tagged browser tests **must fail** if WebGPU is unavailable (instead of skipping). Accepts `1/0/true/false`. | `0` | `scripts/test-all.sh`, Playwright specs (`tests/**`) | `AERO_REQUIRE_WEBGPU=1 npm run test:e2e` |
| `VITE_DISABLE_COOP_COEP` | Disable COOP/COEP response headers on dev/preview servers (useful for validating the non-`SharedArrayBuffer` fallback). Accepts `1/0/true/false`. | `0` | `vite.harness.config.ts`, `web/vite.config.ts`, `web/scripts/serve.cjs`, `server/poc-server.mjs` | `VITE_DISABLE_COOP_COEP=1 npm run dev` |

## Backend / proxy URL variables (used by networking tooling)

These are not required for the core build/test pipeline, but are common when running local networking relays.

| Variable | Meaning | Default | Consumed by | Examples |
| --- | --- | --- | --- | --- |
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
