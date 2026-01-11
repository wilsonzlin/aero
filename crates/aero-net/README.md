# `aero-net` (legacy)

This crate contains the original Tokio-era in-browser networking stack implementation.

It has been **retired** in favor of the maintained networking stacks:

- `crates/aero-net-stack` (Phase 0 in-browser slirp/NAT stack)
- `crates/aero-l2-proxy` (Option C L2-tunnel proxy stack)

`crates/aero-net` is kept in-tree for reference only and is **not** part of the Cargo workspace,
so it is not built, linted, or tested in CI.

