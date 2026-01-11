# aero-gateway-rs (legacy / diagnostic)

This directory contains a legacy Rust/Axum gateway prototype that only implements the historical
`/tcp?target=<host>:<port>` WebSocket tunnel (plus a small `/admin` surface and optional on-disk
capture).

It is **not** the production Aero Gateway contract.

The maintained, CI-tested gateway implementation lives in `backend/aero-gateway/` (Node/TypeScript),
and the public contract is documented in:

- `docs/backend/01-aero-gateway-api.md`
- `docs/backend/openapi.yaml`

## Security warning

This prototype is **not production-hardened** (no session cookie/auth contract, no origin/CORS
enforcement, no destination allow/deny policy, etc.). Treat it as an **unsafe open proxy** and do
not expose it to untrusted networks.

## Run

```bash
cd tools/aero-gateway-rs
cargo run --locked
```

This tool is intentionally **not** a Rust workspace member (see the repo root `Cargo.toml`) so it
does not increase default `cargo build/test` surface area. Build/run it explicitly from this
directory.

Environment variables:

- `AERO_GATEWAY_BIND_ADDR` (default: `127.0.0.1:8080`)
- `ADMIN_API_KEY` (enables `/admin/*`)
- `CAPTURE_DIR`, `CAPTURE_MAX_BYTES`, `CAPTURE_MAX_FILES` (enables capture)
