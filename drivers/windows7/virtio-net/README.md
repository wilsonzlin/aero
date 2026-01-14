# aero_virtio_net (virtio-net NDIS 6.20 miniport for Windows 7 SP1)

This directory contains a clean-room, spec-based **virtio-net** driver for **Windows 7 SP1** implemented as an **NDIS 6.20** miniport.

> **AERO-W7-VIRTIO contract v1:** this driver supports **virtio-pci modern** (virtio 1.0+) using a **BAR0 MMIO** layout and PCI interrupts:
> **INTx required**, with optional **MSI/MSI-X** (message-signaled) when enabled via INF. It binds to `PCI\VEN_1AF4&DEV_1041&REV_01`.
>
> **BAR0 layout validation (strict vs permissive):** by default the driver enforces the contract v1 **fixed BAR0 offsets** (§1.4).
> Developers can disable fixed-offset enforcement at build time (useful for compatibility testing / diagnosing layout issues) by defining:
>
> - `AERO_VIRTIO_MINIPORT_ENFORCE_FIXED_LAYOUT=0`
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
- Feature negotiation:
  - Required: `VIRTIO_F_VERSION_1`, `VIRTIO_F_RING_INDIRECT_DESC`, `VIRTIO_NET_F_MAC`, `VIRTIO_NET_F_STATUS`
  - Optional (wanted):
    - `VIRTIO_F_RING_EVENT_IDX` (opportunistic: suppress kicks/interrupts when supported)
    - `VIRTIO_NET_F_CSUM`, `VIRTIO_NET_F_GUEST_CSUM` (checksum offloads / RX checksum reporting)
    - `VIRTIO_NET_F_GSO`, `VIRTIO_NET_F_HOST_TSO4`, `VIRTIO_NET_F_HOST_TSO6` (TSO/LSO)
    - `VIRTIO_NET_F_HOST_ECN` (ECN/CWR semantics for TSO; uses `virtio_net_hdr.gso_type` ECN bit)
    - `VIRTIO_NET_F_MRG_RXBUF` (mergeable receive buffers; use 12-byte RX/TX header and allow RX frames to span multiple buffers)
    - `VIRTIO_NET_F_CTRL_VQ` + `VIRTIO_NET_F_CTRL_MAC_ADDR` / `VIRTIO_NET_F_CTRL_VLAN` (optional control virtqueue for runtime MAC/VLAN commands)
- Virtqueues:
  - 1 RX/TX queue pair (queue 0 RX, queue 1 TX)
  - Optional control virtqueue (queue index = the device’s last queue; commonly queue 2) when `VIRTIO_NET_F_CTRL_VQ` is negotiated
- Interrupts:
  - INTx (via virtio ISR status register; read-to-ack, spurious-safe)
  - Optional MSI/MSI-X (message-signaled) when enabled via INF. The driver programs virtio MSI-X vectors for config/RX/TX
    and falls back to sharing vector 0 if Windows grants fewer messages. The optional control virtqueue does not get a
    dedicated MSI-X vector and is serviced via polling.
- TX offloads (optional; when offered by the host and enabled by Windows):
  - TCP/UDP checksum offload (IPv4/IPv6) via `VIRTIO_NET_F_CSUM`
  - TCP segmentation offload (TSO/LSO, IPv4/IPv6) via `VIRTIO_NET_F_HOST_TSO4` / `VIRTIO_NET_F_HOST_TSO6`
    (uses `virtio_net_hdr` GSO fields)
    (uses `virtio_net_hdr` GSO fields)
- RX checksum reporting (optional; when offered by the host and negotiated):
  - When `VIRTIO_NET_F_GUEST_CSUM` is negotiated and the device sets `VIRTIO_NET_HDR_F_DATA_VALID` in the RX `virtio_net_hdr`,
    the driver indicates the checksum success to NDIS via `TcpIpChecksumNetBufferListInfo` so the Windows stack can skip software
    checksum validation.

## Optional/Compatibility Features

This section documents behavior that is **not required by AERO-W7-VIRTIO contract v1**, but is relevant when running
against non-contract virtio-net implementations (for example, stock QEMU).

### Net offloads (CSUM/TSO)

Contract v1 Aero device models MUST NOT offer any checksum/GSO/TSO offload features (§3.2.3), but other virtio-net
implementations (notably QEMU) may.

When the host offers them and Windows enables NDIS offloads, this driver can negotiate and use:

- `VIRTIO_NET_F_CSUM`
- `VIRTIO_NET_F_GSO`
- `VIRTIO_NET_F_GUEST_CSUM` (RX checksum status reporting via `virtio_net_hdr.flags`)
- `VIRTIO_NET_F_HOST_TSO4`
- `VIRTIO_NET_F_HOST_TSO6`
- `VIRTIO_NET_F_HOST_ECN` (for correct CWR handling when segmenting TSO packets)

How to validate (in-tree harness):

- Run the Win7 QEMU harness (`drivers/windows7/tests/host-harness/`) and inspect the guest marker
  `AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|...` (and the mirrored host marker `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|...`).
- The marker includes deterministic large transfer diagnostics (`large_*`, `upload_*`) which can be used to compare
  throughput and integrity across configurations.

### Mergeable receive buffers (MRG_RXBUF)

Contract v1 Aero device models MUST NOT offer `VIRTIO_NET_F_MRG_RXBUF` (§3.2.3), but other virtio-net implementations
(notably QEMU) may.

When `VIRTIO_NET_F_MRG_RXBUF` is offered, this driver can negotiate it and will:

- Switch to the 12-byte `struct virtio_net_hdr_mrg_rxbuf` on **both RX and TX**.
- Read `num_buffers` from the first RX header and support a single received frame spanning multiple posted RX buffers
  (reassembled via a chained MDL into one indicated NBL).

### MSI / MSI-X interrupts

On Windows 7, message-signaled interrupts (MSI/MSI-X) are typically **opt-in via INF**. MSI/MSI-X is an optional
enhancement over the contract-required INTx path: it reduces shared line interrupt overhead and can enable per-queue
vectoring.

#### INF registry keys

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
- Even when `MSISupported=1` is set, Windows may still assign only a legacy **INTx** interrupt resource (for example when MSI/MSI-X allocation fails). In that case the driver uses the INTx + ISR read-to-ack path.

#### Expected vector mapping

When MSI-X is available and Windows grants enough messages, the driver uses:

- **Vector/message 0:** virtio **config** interrupt (`common_cfg.msix_config`)
- **Vector/message 1:** queue 0 (`rxq`)
- **Vector/message 2:** queue 1 (`txq`)

If Windows grants fewer than `3` messages (config + RX + TX), the driver falls back to:

- **Vector/message 0:** config + RX + TX
- The optional **control virtqueue** (when negotiated) does not get a dedicated MSI-X vector; the driver disables its MSI-X routing and polls for completions.

#### Troubleshooting / verifying MSI is active

In **Device Manager** (`devmgmt.msc`) → device **Properties** → **Resources**:

- **INTx** typically shows a single small IRQ number (e.g. `IRQ 17`) and may be **shared**.
- **MSI/MSI-X** typically shows one or more interrupt entries with larger values (often shown in hex) and they are usually **not shared**.

You can also use `aero-virtio-selftest.exe`:

- The selftest logs to `C:\\aero-virtio-selftest.log` and emits `AERO_VIRTIO_SELFTEST|TEST|virtio-net|...` markers on stdout/COM1.
- The virtio-net per-test marker includes best-effort interrupt allocation fields:
  - `msi=...|msi_messages=...` (whether Windows assigned message-signaled interrupts and how many)
  - `irq_mode=...|irq_message_count=...|irq_reason=...` (generic IRQ fields; mirrored by the host harness into `VIRTIO_NET_IRQ`)
- The selftest also emits a `virtio-net-irq|INFO|...` line indicating which interrupt mode Windows assigned:
  - `virtio-net-irq|INFO|mode=intx`
  - `virtio-net-irq|INFO|mode=msi|messages=<n>` (message-signaled interrupts; MSI/MSI-X)
- To force MSI-X in the in-tree QEMU harness (and optionally fail if MSI-X is not enabled):
  - Host (best-effort, global): `-VirtioMsixVectors N` / `--virtio-msix-vectors N`
  - Host (best-effort, virtio-net only): `-VirtioNetVectors N` / `--virtio-net-vectors N`
  - Host (hard requirement): `-RequireVirtioNetMsix` / `--require-virtio-net-msix`
  - See `../tests/guest-selftest/README.md` for how to build/run the tool.

See also: [`docs/windows/virtio-pci-modern-interrupt-debugging.md`](../../../docs/windows/virtio-pci-modern-interrupt-debugging.md).

### User-mode diagnostics interface (`\\.\AeroVirtioNetDiag`)

For deterministic debugging/CI scraping, the driver exposes a lightweight, read-only user-mode diagnostics device:

- Win32 device path: `\\.\AeroVirtioNetDiag`
- IOCTL: `AEROVNET_DIAG_IOCTL_QUERY` (`0x00226000`)
- Output struct: `AEROVNET_DIAG_INFO` (packed, versioned)

The IOCTL contract is defined in a shared, WDK-free header so both the kernel driver and the guest selftest can include it:

- `include/aero_virtio_net_diag.h`

The query reports:

- Offered/negotiated virtio feature bits (`HostFeatures`, `GuestFeatures`)
- Interrupt mode (INTx vs MSI/MSI-X), message count, and the programmed virtio MSI-X vectors
  (`msix_config` + `queue_msix_vector` for RX/TX)
- Queue sizes and ring indices, plus virtqueue error flags (corruption indicators)
- Offload enablement toggles (NDIS-controlled) and ctrl-vq state/counters (when negotiated)

Notes:

- The interface is **diagnostic-only** (no writable IOCTLs).
- Callers must validate the returned byte count: the driver may return a truncated struct for compatibility.

### TX cancellation diagnostics (DBG builds)

When built with `DBG`, the miniport tracks how many sends were cancelled at each stage of the TX pipeline:

- `before_sg`: cancelled while awaiting NDIS SG mapping (`MiniportCancelSend` hit while the request is in `TxAwaitingSgList`)
- `after_sg`: cancelled after SG mapping completed but before submission to the virtqueue (`TxPendingList`)
- `after_submit`: cancellation requested after the TX was already submitted to the device (`TxSubmittedList`, best-effort only)

These counters are printed during shutdown/teardown (`AerovNetVirtioStop`) as:

```
aero_virtio_net: tx cancel stats: before_sg=<n> after_sg=<n> after_submit=<n>
```

### Control virtqueue VLAN filter configuration (optional)

When the host offers `VIRTIO_NET_F_CTRL_VQ` + `VIRTIO_NET_F_CTRL_VLAN`, the driver can program the device's VLAN
filter table via the control virtqueue.

The driver does **not** currently expose a Windows-standard VLAN configuration surface (for example, via an NDIS OID).
Instead, it supports a simple **best-effort** per-device registry configuration:

- Registry key (per device instance):
  - `HKLM\\SYSTEM\\CurrentControlSet\\Enum\\<PNP_INSTANCE>\\Device Parameters\\AeroVirtioNet`
- Value:
  - `VlanId` (`REG_DWORD`)
  - Valid range: `1..4094`

On device start (after `DRIVER_OK`), if `VlanId` is present and the required virtio-net features were negotiated, the
driver sends:

- `VIRTIO_NET_CTRL_VLAN_ADD` with the configured VLAN ID

Notes:

- This is optional/compatibility behaviour and is **not required** by the AERO-W7-VIRTIO contract.
- Changes take effect on the next device restart (disable/enable the adapter or reboot).
- If the host device model does not implement VLAN filtering, the command is ignored/failed and the driver continues
  to operate normally.

## Files

- `src/aero_virtio_net.c` – NDIS miniport implementation + virtio-net datapath
- `include/aero_virtio_net.h` – driver-local definitions
- `include/virtio_net_hdr_offload.h` + `src/virtio_net_hdr_offload.c` – portable virtio-net header/L2/L3/L4 parsing helpers (host-testable)
- `src/aero_virtio_net_offload.c` + `include/aero_virtio_net_offload.h` – portable TX header builder used by the miniport
- `inf/aero_virtio_net.inf` – network class INF for installation on Win7 x86/x64
- `tests/host/` – host-side unit tests for the portable offload logic (buildable on Linux/macOS via CMake)

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
