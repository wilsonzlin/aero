# aero_virtio_net (virtio-net NDIS 6.20 miniport for Windows 7 SP1)

This directory contains a clean-room, spec-based **virtio-net** driver for **Windows 7 SP1** implemented as an **NDIS 6.20** miniport.

> **AERO-W7-VIRTIO contract v1:** this driver supports **virtio-pci modern** (virtio 1.0+) using a fixed **BAR0 MMIO** layout and **INTx**
> interrupts and binds to `PCI\VEN_1AF4&DEV_1041&REV_01`.
>
> When using QEMU, pass:
> - `disable-legacy=on` (ensures the device enumerates as `DEV_1041`)
> - `x-pci-revision=0x01` (ensures the device enumerates as `REV_01`)
>
> See [`docs/windows7-virtio-driver-contract.md`](../../../docs/windows7-virtio-driver-contract.md) (§3.2).

## What it provides

- Presents a standard Ethernet NIC to Windows (NDIS 6.20)
- Backs TX/RX using **virtio-net split virtqueues** (virtio 1.0+ **modern** virtio-pci, BAR0 MMIO transport)
- Uses shared Windows 7 virtio helpers from `drivers/windows7/virtio/common/`:
  - `virtio_pci_modern_miniport.{c,h}` (miniport modern transport shim)
  - `virtqueue_split_legacy.{c,h}` (split ring implementation)

## Features (minimal bring-up)

- Virtio handshake: `RESET → ACK → DRIVER → FEATURES_OK → DRIVER_OK`
- Feature negotiation (minimal):
  - Required: `VIRTIO_F_VERSION_1`, `VIRTIO_F_RING_INDIRECT_DESC`, `VIRTIO_NET_F_MAC`, `VIRTIO_NET_F_STATUS`
- 1 RX/TX queue pair (queue 0 RX, queue 1 TX)
- INTx interrupt path (via virtio ISR register; read-to-ack). MSI-X is intentionally disabled; INTx is required.
- No checksum offloads / TSO / LRO

## Files

- `src/aero_virtio_net.c` – NDIS miniport implementation + virtio-net datapath
- `include/aero_virtio_net.h` – driver-local definitions
- `inf/aero_virtio_net.inf` – network class INF for installation on Win7 x86/x64

## Building

### Supported: WDK10 / MSBuild (CI path)

CI builds this driver via the MSBuild project:

- `drivers/windows7/virtio-net/aero_virtio_net.vcxproj`

From a Windows host with the WDK installed:

```powershell
# From the repo root:
.\ci\install-wdk.ps1
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json -Drivers windows7/virtio-net
```

Build outputs are staged under:

- `out/drivers/windows7/virtio-net/x86/aero_virtio_net.sys`
- `out/drivers/windows7/virtio-net/x64/aero_virtio_net.sys`

To stage an installable/signable package, copy the built SYS into the package staging folder:

```text
drivers/windows7/virtio-net/inf/aero_virtio_net.sys
```

### Legacy/deprecated: WDK 7.1 `build.exe`

For local development you can also use the legacy WinDDK 7600 `build` utility (`sources`/`makefile` are kept for that workflow).

## Installing on Windows 7

1. Ensure the VM exposes a virtio-net PCI device (e.g. QEMU `-device virtio-net-pci,...`).
2. Copy `inf/aero_virtio_net.inf` and `aero_virtio_net.sys` into the **same directory** on the guest.
3. Install using Device Manager → Update Driver, pointing at `aero_virtio_net.inf`.
4. Windows 7 x64 requires signed drivers unless **test signing** is enabled.

Hardware IDs matched by `inf/aero_virtio_net.inf`:

- `PCI\VEN_1AF4&DEV_1041&REV_01` (virtio-net modern, Aero contract v1)

Note: This driver uses the virtio-pci **modern MMIO** transport and does not implement the legacy I/O-port register map.
