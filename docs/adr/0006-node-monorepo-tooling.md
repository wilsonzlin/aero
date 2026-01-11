# ADR 0006: Node monorepo tooling (npm workspaces + single lockfile)

## Context

The repository contains multiple Node packages (`web/`, `backend/aero-gateway/`, `net-proxy/`, `tools/perf/`, etc.).
Historically, each package carried its own `package-lock.json` and was installed/tested independently in CI.

This caused:

- dependency/version drift (e.g. multiple Playwright / TypeScript versions in one repo)
- slow CI (multiple installs + caches)
- confusing local workflows (`npm ci` in several directories)

We want a coherent monorepo with a single, predictable install story.

## Decision

Adopt **npm workspaces** with a **single root `package-lock.json`**.

- The repository root `package.json` declares all Node workspaces.
- The root `package-lock.json` is the *only* lockfile for workspace packages.
- CI and local workflows use `npm ci` from the repo root.
- Package-scoped commands are run via npm workspaces:
  - `npm -w web run dev`
  - `npm -w backend/aero-gateway test`

### Workspace membership

Workspace packages include (at minimum):

- `web/`
- `backend/aero-gateway/`
- `net-proxy/`
- `tools/perf/`
- `packages/*`

Additional existing Node packages are also treated as workspaces (for consistent dependency resolution and to avoid stray lockfiles):

- `services/image-gateway/`
- `tools/disk-streaming-browser-e2e/`
- `tools/net-proxy-server/`
- `tools/range-harness/`
- `bench/`
- `server/` (**legacy**, but kept in the workspace to eliminate a separate lockfile)
- `proxy/webrtc-udp-relay/e2e/` (Playwright E2E harness; still runnable independently, but shares the repo lockfile)

## Alternatives considered

1. **pnpm workspaces**
   - Pros: fast installs, good dedupe, `--filter` support for scoped installs in CI.
   - Cons: adds a new package manager + lockfile, more migration work, and requires tooling changes across workflows.

2. **Yarn Berry**
   - Pros: strong workspace support, constraints, and plug'n'play options.
   - Cons: larger behavioral change, more contributor friction, and a more complex migration.

We chose **npm workspaces** because the repo already standardizes on npm + `package-lock.json`, npm workspaces are good enough for our current needs, and they integrate cleanly with existing CI and dependabot configuration.

## Consequences

- **Single command installs everything:** `npm ci` at the repo root is the only supported CI install.
- **Fewer mismatches:** shared tooling versions (TypeScript, Playwright, Vitest) are aligned by construction.
- **Simpler CI caching:** workflows cache via the root `package-lock.json` instead of per-package lockfiles.
- **Workspace-first developer workflow:** contributors should run scripts via `npm -w <path> …` instead of `cd <dir> && npm …`.

