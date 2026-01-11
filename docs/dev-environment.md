# Reproducible development environment

Aero’s canonical workflows assume a small set of native tooling (Rust stable + nightly, Node, QEMU, etc.). To make “clone → build → test” reliable, this repo ships a **VS Code / GitHub Codespaces dev container** definition.

## Option A: Dev Container (recommended)

### Prerequisites (host)

- Docker Desktop / Docker Engine
- VS Code + the “Dev Containers” extension (or GitHub Codespaces)

### Usage

1. Clone the repo.
2. Open it in VS Code.
3. Run **“Dev Containers: Reopen in Container”**.

The container image installs:

- Rust toolchains: **stable + nightly**
  - `wasm32-unknown-unknown` target for both
  - `rust-src` for nightly (required for `-Z build-std` threaded/shared-memory WASM builds)
- Node.js **v20.11.1** (matching CI’s pinned Node 20.x version)
- `wasm-pack`
- Binaryen (`wasm-opt`)
- QEMU + boot-test deps: `qemu-system-x86`, `mtools`, `nasm`, `unzip`
- ACPICA tools: `iasl` (`acpica-tools`)
- Playwright runtime dependencies (browser binaries are installed separately; see below)

It also applies the recommended environment defaults from [`scripts/agent-env.sh`](../scripts/agent-env.sh) (job parallelism, Node heap cap, Playwright worker count) and attempts to bump `ulimit -n` to avoid “too many open files” failures.

### Validating your setup

From the repo root **inside the container**:

```bash
just setup
cargo xtask test-all --skip-e2e
```

By default, `cargo xtask test-all` uses the **repo root** Node workspace (`package.json` in the root) which matches CI.
If you want to run tests against a different Node workspace (e.g. `web/`), override it:

```bash
AERO_NODE_DIR=web cargo xtask test-all --skip-e2e
```

### Running Playwright E2E tests (optional)

The container includes the native libraries Playwright needs, but it does **not** pre-download browser binaries.

Install the browsers once:

```bash
npx playwright install chromium
```

Then run:

```bash
cargo xtask test-all
```

## Option B: Nix (optional)

This repo currently does not ship a Nix flake. If you’d like to contribute one, it should provide a `devShell` with the same toolchain as the dev container and document the equivalent validation steps (`just setup` + `cargo xtask test-all --skip-e2e`).
