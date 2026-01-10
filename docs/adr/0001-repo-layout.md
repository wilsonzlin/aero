# ADR 0001: Repository layout (Rust workspace + `web/` Vite app)

## Context

Aero is split between:

- A performance-critical emulator core (Rust compiled to WebAssembly).
- A browser “host” application (TypeScript/HTML/CSS) that provides UI, wiring to browser APIs, workers, and deployment tooling.

We need a repo layout that:

- Keeps Rust crates modular and testable (workspace ergonomics).
- Keeps the browser app experience modern (fast dev server, HMR, bundling, worker support).
- Makes it straightforward to ship multiple WebAssembly build variants (threaded vs single-threaded).

## Decision

Adopt a **Rust workspace at the repository root** with a dedicated **`web/` Vite app** for the browser host:

- Root: `Cargo.toml` with `[workspace]` members for all Rust crates.
- Rust crates live under `crates/` (or equivalent) and produce WebAssembly artifacts consumable by the host.
- The browser app lives under `web/` and uses **Vite** for development and builds.
- The `web/` build consumes the generated WASM + JS glue (e.g., `wasm-bindgen` output) as build inputs.

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
- CI can build/test Rust crates via the workspace and build the host via `web/`.
- WebAssembly artifacts become explicit build outputs that the `web/` app depends on, which simplifies packaging and makes build variants feasible.

