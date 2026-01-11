# `aero-virtio-protocol`

Small, host-buildable crate containing `#[repr(C)]` structs/constants for **virtio** (rings + virtio-pci capabilities + device protocol headers used by Aero).

Why this exists:

- The emulator and the Windows guest drivers must agree on **exact binary layouts**.
- These unit tests are a cheap way to lock down struct sizes/offsets early.

Run tests:

```bash
cargo test --locked --manifest-path drivers/protocol/virtio/Cargo.toml
```
