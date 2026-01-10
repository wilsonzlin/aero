# Win7 virtio portable tests

This directory contains **hardware-free** tests for the Win7 virtio transport's
PCI capability discovery logic.

The tests build synthetic 256-byte PCI config-space images and validate that the
portable Virtio 1.0+ "modern" capability parser:

- walks the PCI capability list safely (detects loops/malformed chains)
- parses `virtio_pci_cap` / `virtio_pci_notify_cap`
- discovers the required modern capabilities (common/notify/isr/device)
- reports deterministic error codes for malformed inputs

## Run

From the repo root:

```bash
./drivers/win7/virtio/tests/build_and_run.sh
```

Optionally pick a compiler:

```bash
CC=clang ./drivers/win7/virtio/tests/build_and_run.sh
```

## Related code

- Parser: `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.{h,c}`
