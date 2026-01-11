# Windows 7 virtio shared code (`drivers/windows7/virtio/common`)

This directory contains shared virtio helper code used by Aero’s in-tree Windows 7 SP1 virtio drivers
that implement the **AERO-W7-VIRTIO contract v1** (virtio-pci **modern** transport + modern PCI Device ID
space `DEV_104x`/`DEV_105x`).

- Contract: `docs/windows7-virtio-driver-contract.md`
- Host/portable virtio helpers (cap parser tests, etc.): `drivers/win7/virtio/`

## Why this matters (INF hardware ID conflicts)

Windows picks drivers by matching PCI hardware IDs against installed INFs. Aero’s
contract v1 uses modern-only IDs like:

- `PCI\VEN_1AF4&DEV_1041&REV_01` (virtio-net)
- `PCI\VEN_1AF4&DEV_1042&REV_01` (virtio-blk)

To avoid Windows seeing **multiple** INFs that match the **same** modern device,
make sure your driver set does **not** contain duplicate INFs that bind the
same modern IDs.

If you have historical/experimental driver copies elsewhere in the repo (for example
under `drivers/win7/`), they must not ship an additional `.inf` that matches the
same contract-v1 HWIDs. If you need multiple candidate INFs for debugging, disambiguate by explicit
relative INF path during provisioning and ensure only one matching INF is present on the guest at a time.

## Driver package locations (canonical)

- virtio-blk: `drivers/windows7/virtio-blk/inf/aero_virtio_blk.inf`
- virtio-net: `drivers/windows7/virtio-net/inf/aero_virtio_net.inf`
- virtio-input: `drivers/windows7/virtio-input/inf/aero_virtio_input.inf`
- virtio-snd: `drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf`

## Contents

- `common/` – shared Windows 7 virtio helpers (INTx helper, contract checks, legacy I/O-port transport, host-test helpers).
  - Used by the Win7 virtio **miniport** drivers (`virtio-blk`, `virtio-net`), which compile:
    - `common/include/virtio_pci_modern_miniport.h` + `common/src/virtio_pci_modern_miniport.c`
    - `common/include/virtqueue_split_legacy.h` + `common/src/virtqueue_split_legacy.c`
  - Other Win7 virtio drivers use shared code under `drivers/windows/virtio/` as well:
    - `virtio-input` and `virtio-snd` compile the canonical split-ring engine: `drivers/windows/virtio/common/virtqueue_split.c`
    - `virtio-snd` also compiles the canonical WDF-free virtio-pci modern transport: `drivers/windows/virtio/pci-modern/`

Note: the contract-v1 driver packages live under:

- `drivers/windows7/virtio-blk/`
- `drivers/windows7/virtio-net/`
- `drivers/windows7/virtio-input/`
- `drivers/windows7/virtio-snd/`

If you are packaging Aero’s contract-v1 drivers, avoid staging multiple INFs that
bind the same modern HWIDs (or share the same basename) unless you explicitly
disambiguate by relative path during provisioning.
