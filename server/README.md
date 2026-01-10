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
