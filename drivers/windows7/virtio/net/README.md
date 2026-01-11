# Aero virtio-net driver (Windows 7 SP1)

This directory contains a clean-room, spec-based **virtio-net** driver for **Windows 7 SP1** implemented as an **NDIS 6.20** miniport.

> **Important:** This directory contains the **legacy/transitional** virtio-net driver
> package. It intentionally binds to `PCI\\VEN_1AF4&DEV_1000` and does **not** bind to
> the contract-v1 modern virtio-net ID `PCI\\VEN_1AF4&DEV_1041`.
>
> For the contract-v1 virtio-net driver, use `drivers/win7/virtio-net/` (see also
> `docs/windows7-virtio-driver-contract.md`).

## What it provides

- Presents a standard Ethernet NIC to Windows (NDIS 6.20)
- Backs TX/RX using **virtio-net split virtqueues** (virtio 1.0+ **modern** virtio-pci, BAR0 MMIO transport)
- Uses the shared virtio-pci modern transport helper in `drivers/windows7/virtio-modern/common/`
- Uses the shared split-ring engine in `drivers/windows/virtio/common/`

## Features (minimal bring-up)

- Virtio handshake: `RESET → ACK → DRIVER → FEATURES_OK → DRIVER_OK`
- Feature negotiation (minimal):
  - `VIRTIO_F_VERSION_1`
  - `VIRTIO_F_RING_INDIRECT_DESC`
  - `VIRTIO_NET_F_MAC`
  - `VIRTIO_NET_F_STATUS` (link state)
- 1 RX/TX queue pair (queue 0 RX, queue 1 TX)
- INTx interrupt path (via virtio ISR register; read-to-ack). MSI-X is intentionally disabled; INTx is required.
- No checksum offloads / TSO / LRO

## Files

- `src/aerovnet.c` – NDIS miniport implementation + virtio-net datapath
- `include/aerovnet.h` – driver-local definitions
- `aerovnet.inf` – network class INF for installation on Win7 x86/x64

## Building

CI builds this driver with a modern WDK (currently pinned to 10.0.22621.0) via the MSBuild project `aerovnet.vcxproj`.

For local development you can use either:

- `aerovnet.vcxproj` (Visual Studio / MSBuild + WDK 10), or
- the legacy WDK 7.1 `build` utility (`sources`/`makefile` are kept for that workflow).

## Installing on Windows 7

1. Ensure the VM exposes a virtio-net PCI device (e.g. QEMU `-device virtio-net-pci,...`).
2. Install using Device Manager → Update Driver, pointing at `aerovnet.inf`.
3. Windows 7 x64 requires signed drivers unless **test signing** is enabled.

Hardware IDs matched by `aerovnet.inf`:

- `PCI\\VEN_1AF4&DEV_1000` (transitional virtio-net)

Note: This driver uses the virtio-pci **modern MMIO** transport and does not implement the legacy I/O-port register map.
