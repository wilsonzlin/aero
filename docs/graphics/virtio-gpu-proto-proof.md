# virtio-gpu-proto proof

This repo includes a narrow virtio-gpu 2D scanout prototype in `crates/virtio-gpu-proto`.

## Minimal automated validation

Command:

```bash
cargo test -p virtio-gpu-proto
```

Expected output (trimmed):

```text
running 1 test
test basic_2d_scanout_roundtrip ... ok

test result: ok. 1 passed; 0 failed
```

What the test covers:

- a simulated “guest memory” buffer contains BGRA pixels
- virtio-gpu commands create a 2D resource, attach the backing, transfer pixels, bind scanout, and flush
- the device’s scanout buffer is byte-for-byte equal to the original guest pixels
- exercises multi-entry (scatter/gather) backing, partial rect updates, and a simple mode switch via `SET_SCANOUT`

Additional covered behaviors (unit-tested indirectly via the command flow):

- `GET_EDID` returns a minimal EDID blob (transport test)
- BGRX formats are accepted and coerced to opaque alpha on upload

## Virtio transport integration proof

The same scanout sequence is also tested *through* a real virtio-pci + split-virtqueue transport:

```bash
cargo test -p aero-virtio virtio_gpu_2d_scanout_via_virtqueue
```
