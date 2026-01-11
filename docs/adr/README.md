# Architecture Decision Records (ADRs)

This directory contains **Architecture Decision Records**: short, versioned documents
that capture *why* we made a project-wide infrastructure decision, what alternatives
were considered, and what the consequences are.

## Index

- [ADR 0001: Repository layout (Rust workspace + `web/` Vite app)](./0001-repo-layout.md)
- [ADR 0002: Cross-origin isolation (COOP/COEP) for threads + SharedArrayBuffer](./0002-cross-origin-isolation.md)
- [ADR 0003: Shared memory layout (multiple SABs; WASM 4 GiB constraint)](./0003-shared-memory-layout.md)
- [ADR 0004: WebAssembly build variants (threaded vs single-threaded; runtime selection)](./0004-wasm-build-variants.md)
- [ADR 0005: Commit `Cargo.lock` for reproducible Rust builds](./0005-cargo-lock-policy.md)
- [ADR 0006: Node monorepo tooling (npm workspaces + single lockfile)](./0006-node-monorepo-tooling.md)
- [ADR 0009: Rust toolchain policy (pinned stable + pinned nightly for threaded WASM)](./0009-rust-toolchain-policy.md)
- [ADR 0010: Canonical audio stack (aero-audio + aero-virtio; legacy emulator audio gated)](./0010-canonical-audio-stack.md)

## Creating a new ADR

1. Pick the next number (`0011-...`).
2. Use a descriptive slug (`kebab-case`).
3. Include these sections:
   - **Context**
   - **Decision**
   - **Alternatives considered**
   - **Consequences**
