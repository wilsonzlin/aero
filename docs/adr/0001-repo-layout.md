# ADR 0001: Repository layout (Rust workspace + repo-root Vite app)

## Context

Aero is split between:

- A performance-critical emulator core (Rust compiled to WebAssembly).
- A browser “host” application (TypeScript/HTML/CSS) that provides UI, wiring to browser APIs, workers, and deployment tooling.

We need a repo layout that:

- Keeps Rust crates modular and testable (workspace ergonomics).
- Keeps the browser app experience modern (fast dev server, HMR, bundling, worker support).
- Makes it straightforward to ship multiple WebAssembly build variants (threaded vs single-threaded).

## Decision

Adopt a **Rust workspace at the repository root** with a **repo-root Vite app** for the browser host:

- Root: `Cargo.toml` with `[workspace]` members for all Rust crates.
- Rust crates live under `crates/` (or equivalent) and produce WebAssembly artifacts consumable by the host.
- The **canonical browser host app** lives at the repo root:
  - `index.html`
  - `src/`
  - `vite.harness.config.ts` (Vite config; also used by Playwright)
- The `web/` directory is **not** the canonical host app. It primarily contains:
  - shared runtime modules imported by the repo-root app (`web/src/...`)
  - WASM build tooling (`web/scripts/build_wasm.mjs`, `npm -w web run wasm:build`)
  - a legacy/experimental Vite entrypoint (`web/index.html`) that can be served under `/web/` by the repo-root app

This layout makes “Rust builds” and “web builds” first-class while still living in a single repository.

## Alternatives considered

1. **Single-package Rust repo with inlined web assets**
   - Pros: fewer moving parts.
   - Cons: poor frontend DX; difficult worker setup; awkward asset pipeline.

2. **Separate repositories (Rust core repo + web host repo)**
   - Pros: strong separation of concerns.
   - Cons: version skew risk; harder cross-cutting refactors; more CI complexity.

3. **Non-Vite tooling (Trunk / webpack / bespoke scripts)**
   - Pros: can work.
   - Cons: Vite has the best combination of worker ergonomics, speed, and mainstream familiarity.

## Consequences

- Contributors can work on Rust and the web host independently, but still in one repo.
- CI and developer tooling run the browser host from the repo root (`npm run dev`, `just dev`).
- WebAssembly artifacts become explicit build outputs that the repo-root app depends on (`web/src/wasm/pkg-*`), which simplifies packaging and makes build variants feasible.
