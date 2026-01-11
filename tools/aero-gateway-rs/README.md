# aero-gateway-rs (legacy)

This directory contains the legacy Rust/Axum gateway prototype that only implements the historical
`/tcp?target=<host>:<port>` WebSocket tunnel (plus a small `/admin` surface and optional on-disk
capture).

It is **not** the production Aero Gateway contract.

The maintained, CI-tested gateway implementation lives in `backend/aero-gateway/` (Node/TypeScript),
and the public contract is documented in:

- `docs/backend/01-aero-gateway-api.md`
- `docs/backend/openapi.yaml`

## Run

```bash
cargo run -p aero-gateway-rs
```

Environment variables:

- `AERO_GATEWAY_BIND_ADDR` (default: `127.0.0.1:8080`)
- `ADMIN_API_KEY` (enables `/admin/*`)
- `CAPTURE_DIR`, `CAPTURE_MAX_BYTES`, `CAPTURE_MAX_FILES` (enables capture)
