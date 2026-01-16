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
2. **Disk origin** – the real reference `server/disk-gateway` (Rust) started via `cargo run --locked` and configured to serve:
   - Public image id `win7` → `GET /disk/win7` (Range-capable)
   - Private image id `secret` for user `alice` → `POST /api/images/secret/lease` + `GET /disk/secret` with `Authorization: Bearer <token>`

## Running locally

```bash
# From the repo root (npm workspaces)
npm ci

# Run headless tests
npm -w tools/disk-streaming-browser-e2e test
```

Prerequisites:

- A Rust toolchain (`cargo`) capable of building `server/disk-gateway`.
- On minimal Linux environments, Playwright may require extra system packages. If `npm test` fails with
  missing shared libraries, install them via:
  ```bash
  node scripts/playwright_install.mjs chromium --with-deps
  ```

## Fixtures

Deterministic binary fixtures are committed under `./fixtures/` so byte comparisons are stable.
These are **synthetic byte patterns** (not Windows media):

- `fixtures/win7.img` (public)
- `fixtures/secret.img` (private)
