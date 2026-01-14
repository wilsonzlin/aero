# Aero backend server (legacy)

This `server/` package was an earlier monolithic backend for Aero (static hosting + TCP proxy + DNS lookup).

**It is being superseded by the Aero Gateway backend contract**:

- Implementation: `backend/aero-gateway`
- API contract: [`docs/backend/01-aero-gateway-api.md`](../docs/backend/01-aero-gateway-api.md)
- OpenAPI (HTTP endpoints): [`docs/backend/openapi.yaml`](../docs/backend/openapi.yaml)

For browser-side requirements (COOP/COEP / cross-origin isolation), see:

- [`docs/11-browser-apis.md`](../docs/11-browser-apis.md#cross-origin-isolation-coopcoep-deployment-requirements)
- [`docs/deployment.md`](../docs/deployment.md)
- [`docs/security-headers.md`](../docs/security-headers.md)

## Legacy protocol docs

If you need the old endpoints/protocols for historical prototypes, see [`server/LEGACY.md`](./LEGACY.md).

## Dev helpers (disk streaming)

This directory also contains standalone dev-only helpers used by the disk streaming conformance tooling:

- `server/range_server.js`: static file server with HTTP Range + CORS headers
  - Used by `tools/disk-streaming-conformance/selftest_range_server.py`
- `server/chunk_server.js`: static server for chunked disk images (`manifest.json` + `chunks/*.bin`)
  - Used by `tools/disk-streaming-conformance/selftest_chunk_server.py`
