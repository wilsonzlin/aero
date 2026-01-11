# Repo layout (canonical vs legacy/prototypes)

This repo contains multiple generations of frontend/backend work. The goal is to make it obvious where **new changes should go** and to keep CI/dev tooling from accidentally targeting the wrong thing.

For project-wide layout decisions, see: [`docs/adr/0001-repo-layout.md`](./adr/0001-repo-layout.md).

## Canonical / production paths

### Browser host app (production): `web/` (Vite)

The **production** browser host app lives in:

- `web/` (Vite app)
- Source: `web/src/`
- Config: `web/vite.config.ts`

Recommended dev workflow from the repo root:

```bash
just setup
just dev
```

### Rust emulator workspace: root `Cargo.toml` + `crates/`

The Rust codebase is a workspace rooted at:

- `Cargo.toml` (workspace)
- `crates/` (workspace members)

#### Crate naming convention (important)

Crates should use `aero-foo` **kebab-case** package names and matching `crates/aero-foo/`
directories. Note that Rust `use` paths still normalize `-` → `_` (e.g. `aero-cpu-core` is
imported as `aero_cpu_core`).

This repo still contains some older crates that are not `aero-*` prefixed (e.g. `crates/emulator`,
`crates/memory`). These remain in the workspace for now, but **new crates should follow the
convention**.

See [`docs/adr/0007-rust-crate-naming.md`](./adr/0007-rust-crate-naming.md).

#### Graphics ABI note (AeroGPU)

There are multiple GPU “protocols” in-tree from different phases of bring-up. New work intended
for the Windows 7 WDDM graphics path should target the canonical AeroGPU ABI in:

- `drivers/aerogpu/protocol/*` (C headers, source of truth)
- `emulator/protocol` (Rust/TypeScript mirror)
- `crates/emulator/src/devices/pci/aerogpu.rs` (emulator device model)

Some legacy/prototype GPU ABIs have existed during bring-up and are **not** the Win7/WDDM driver
contract.

See `docs/graphics/aerogpu-protocols.md` for the full mapping.

### Backend services (production)

Most maintained backend work lives under:

- `backend/` (e.g. `backend/aero-gateway`)
- `services/` (deployment-oriented services)

## Non-canonical / quarantined paths

### Repo-root Vite app: *dev/test harness* (not production)

The repo root still contains a small Vite entrypoint used for debugging and browser automation:

- `index.html`
- `src/main.ts`
- `vite.harness.config.ts`

This is **not** the production host app. It exists so Playwright (and other tooling) can:

- run debug panels and smoke tests without depending on the production UI surface
- import repo modules via paths like `/web/src/...` from a single dev server root

Use explicitly:

```bash
npm run dev:harness
```

### Legacy backend: `server/`

`server/` is a legacy Node backend (static hosting + early TCP proxy). New work should target `backend/aero-gateway`.

See: `server/LEGACY.md`.

### Prototypes / PoCs

These directories are intentionally **not** production code:

- `poc/` – small proof-of-concepts (usually referenced from docs)
- `prototype/` – larger prototypes / RFC companions
- `prototype/legacy-win7-aerogpu-1ae0/` – legacy Win7 AeroGPU prototype stack (deprecated; archived)

If you add new experiments, keep them under one of these (or a clearly named `legacy/` directory) and document them with a small `README.md`.
