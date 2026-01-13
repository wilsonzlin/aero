# aero_virtio_net (virtio-net NDIS 6.20 miniport for Windows 7 SP1)

This directory contains a clean-room, spec-based **virtio-net** driver for **Windows 7 SP1** implemented as an **NDIS 6.20** miniport.

> **AERO-W7-VIRTIO contract v1:** this driver supports **virtio-pci modern** (virtio 1.0+) using a fixed **BAR0 MMIO** layout and PCI interrupts:
> **INTx required**, with optional **MSI/MSI-X** (message-signaled) when enabled via INF. It binds to `PCI\VEN_1AF4&DEV_1041&REV_01`.
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
- Interrupts:
  - INTx (via virtio ISR status register; read-to-ack, spurious-safe)
  - Optional MSI/MSI-X (message-signaled) when enabled via INF. The driver programs virtio MSI-X vectors for config/RX/TX and falls back to sharing vector 0 if Windows grants fewer messages.
- No checksum offloads / TSO / LRO

## MSI / MSI-X interrupts (optional)

On Windows 7, message-signaled interrupts (MSI/MSI-X) are typically **opt-in via INF**. MSI/MSI-X is an optional enhancement over the contract-required
INTx path: it reduces shared line interrupt overhead and can enable per-queue vectoring.

### INF registry keys

On Windows 7, MSI/MSI-X is typically opt-in via `HKR` settings under:

`Interrupt Management\\MessageSignaledInterruptProperties`

As shipped in `inf/aero_virtio_net.inf`:

```inf
[AeroVirtioNet_Install.NT.HW]
AddReg = AeroVirtioNet_InterruptManagement_AddReg

[AeroVirtioNet_InterruptManagement_AddReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
; virtio-net needs config + RX + TX = 3 vectors, but request extra for future growth.
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
```

Notes:

- `0x00010001` = `REG_DWORD`
- `MessageNumberLimit` is a **request**, not a guarantee. The driver remains functional with fewer messages and will fall back as described below.

### Expected vector mapping

When MSI-X is available and Windows grants enough messages, the driver uses:

- **Vector/message 0:** virtio **config** interrupt (`common_cfg.msix_config`)
- **Vector/message 1:** queue 0 (`rxq`)
- **Vector/message 2:** queue 1 (`txq`)

If Windows grants fewer than `1 + numQueues` messages, the driver falls back to:

- **All sources on vector/message 0** (config + all queues)

### Troubleshooting / verifying MSI is active

In **Device Manager** (`devmgmt.msc`) → device **Properties** → **Resources**:

- **INTx** typically shows a single small IRQ number (e.g. `IRQ 17`) and may be **shared**.
- **MSI/MSI-X** typically shows one or more interrupt entries with larger values (often shown in hex) and they are usually **not shared**.

See also: [`docs/windows/virtio-pci-modern-interrupt-debugging.md`](../../../docs/windows/virtio-pci-modern-interrupt-debugging.md).

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
