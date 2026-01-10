# Disk streaming browser E2E (COOP/COEP + Range + auth)

This package runs **browser-level** E2E tests (Playwright) that catch the class of failures that HTTP-only tests miss:

- `Cross-Origin-Opener-Policy: same-origin` + `Cross-Origin-Embedder-Policy: require-corp` must produce `window.crossOriginIsolated === true`
- Cross-origin `fetch()` requests (including **Range** requests) must continue to work under COEP
- Private/leased resources must reject requests without a token and accept requests with a valid token

## What it starts

The Playwright suite spins up **two local HTTP origins** on separate ports:

1. **App origin** – serves a minimal HTML page with:
   - `Cross-Origin-Opener-Policy: same-origin`
   - `Cross-Origin-Embedder-Policy: require-corp`
2. **Disk origin** – a small “disk-gateway-like” server that supports:
   - `GET /api/images/:id/lease` → JSON `{ token }`
   - `GET /api/images/:id/bytes` with `Range` support and token auth for private fixtures

> Note: in the full project, this disk origin is expected to be the real `server/disk-gateway`.
> This harness is intentionally written to be “disk-gateway shaped” so it can be swapped over.

## Running locally

```bash
cd tools/disk-streaming-browser-e2e

# Install deps
npm ci

# Run headless tests
npm test
```

## Fixtures

Deterministic binary fixtures are committed under `./fixtures/` so byte comparisons are stable.
