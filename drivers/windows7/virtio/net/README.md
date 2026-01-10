# Aero virtio-net driver (Windows 7 SP1)

This directory contains a clean-room, spec-based **virtio-net** driver for **Windows 7 SP1** implemented as an **NDIS 6.20** miniport.

## What it provides

- Presents a standard Ethernet NIC to Windows (NDIS 6.20)
- Backs TX/RX using **virtio-net split virtqueues** (legacy virtio-pci I/O transport)
- Uses the shared virtio core library in `drivers/windows7/virtio/common/`

## Features (minimal bring-up)

- Virtio handshake: `RESET → ACK → DRIVER → FEATURES_OK → DRIVER_OK`
- Feature negotiation (minimal):
  - `VIRTIO_NET_F_MAC`
  - `VIRTIO_NET_F_STATUS` (link state) when offered by device
- 1 RX/TX queue pair (queue 0 RX, queue 1 TX)
- INTx interrupt path (via virtio ISR register)
- No checksum offloads / TSO / LRO

## Files

- `src/aerovnet.c` – NDIS miniport implementation + virtio-net datapath
- `include/aerovnet.h` – driver-local definitions
- `aerovnet.inf` – network class INF for installation on Win7 x86/x64

## Building

Build with a Windows driver toolchain that can target Windows 7 (e.g. WDK 7.1). The build integration (MSBuild/WDK `sources`) is intentionally not committed here; the driver sources are arranged to be included in either build system.

## Installing on Windows 7

1. Ensure the VM exposes a virtio-net PCI device (e.g. QEMU `-device virtio-net-pci,...`).
2. Install using Device Manager → Update Driver, pointing at `aerovnet.inf`.
3. Windows 7 x64 requires signed drivers unless **test signing** is enabled.

Hardware IDs matched by `aerovnet.inf`:

- `PCI\\VEN_1AF4&DEV_1000` (legacy virtio-net)
- `PCI\\VEN_1AF4&DEV_1041` (virtio 1.0 transitional virtio-net)

