# aero_virtio_blk (virtio-blk StorPort miniport for Windows 7)

`aero_virtio_blk.sys` is a StorPort miniport driver intended for Windows 7 SP1 x86/x64.

> **AERO-W7-VIRTIO contract v1:** this driver binds to the virtio-blk **modern-only**
> PCI ID `PCI\VEN_1AF4&DEV_1042&REV_01` and validates `REV_01` at runtime.
>
> **BAR0 layout validation (strict vs permissive):** by default the driver enforces the contract v1 **fixed BAR0 offsets** (§1.4).
> Developers can disable fixed-offset enforcement at build time (useful for compatibility testing / diagnosing layout issues) by defining:
>
> - `AERO_VIRTIO_MINIPORT_ENFORCE_FIXED_LAYOUT=0`
>
> When using QEMU, pass:
> - `disable-legacy=on` (ensures the device enumerates as `DEV_1042`)
> - `x-pci-revision=0x01` (ensures the device enumerates as `REV_01`)

## What it provides

- Presents a StorPort SCSI adapter to Windows backed by a virtio-blk PCI function.
- Uses shared Windows 7 virtio helpers from `drivers/windows7/virtio/common/`:
  - `virtio_pci_modern_miniport.{c,h}` (miniport modern transport shim)
  - `virtqueue_split_legacy.{c,h}` (split ring implementation)

## Optional/Compatibility Features

### Interrupts (INTx vs MSI/MSI-X)

Per the [`AERO-W7-VIRTIO` v1 contract](../../../docs/windows7-virtio-driver-contract.md) (§1.8), **INTx is required** and MSI/MSI-X is an optional enhancement.

The miniport supports both:

- **INTx (line-based)** — legacy virtio-pci interrupt semantics using the ISR status byte (read-to-ack).
- **MSI/MSI-X (message-signaled)** — when Windows assigns message interrupts, the driver programs the virtio MSI-X routing registers (`msix_config` / `queue_msix_vector`) and services completions without relying on ISR status.

On Windows 7, message-signaled interrupts are opt-in via INF. The shipped `inf/aero_virtio_blk.inf` requests MSI/MSI-X
and Windows will fall back to INTx when MSI isn't available.

#### INF registry keys

The MSI opt-in keys live under:

`Interrupt Management\MessageSignaledInterruptProperties`

As shipped in `inf/aero_virtio_blk.inf`:

```inf
[AeroVirtioBlk_Inst.HW]
AddReg = AeroVirtioBlk_Inst_HW_AddReg

[AeroVirtioBlk_Inst_HW_AddReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\MessageSignaledInterruptProperties", "MSISupported", 0x00010001, 1
HKR, "Interrupt Management\MessageSignaledInterruptProperties", "MessageNumberLimit", 0x00010001, 4
```

Notes:

- `0x00010001` = `REG_DWORD`
- `MessageNumberLimit` is a request; Windows may grant fewer messages than requested.

#### Expected vector mapping

When MSI-X is active and Windows grants enough messages, the driver uses:

- **Vector/message 0:** virtio **config** interrupt (`common_cfg.msix_config`)
- **Vector/message 1:** queue 0 (`requestq`)

Fallback when messages are insufficient:

- **All sources on vector/message 0** (config + all queues)

#### Troubleshooting / verifying MSI is active

In **Device Manager** → device **Properties** → **Resources**:

- **INTx** typically shows a single small IRQ number (and may be shared).
- **MSI/MSI-X** typically shows one or more interrupt entries with larger values (often shown in hex) and they are usually not shared.

You can also use `aero-virtio-selftest.exe`:

- The selftest logs to `C:\\aero-virtio-selftest.log` and emits `AERO_VIRTIO_SELFTEST|TEST|virtio-blk|...` markers on stdout/COM1.
- The `AERO_VIRTIO_SELFTEST|TEST|virtio-blk|...` marker includes interrupt diagnostics from the miniport IOCTL query:
  - `irq_mode=<intx|msi|msix>`
  - `msix_config_vector=0x....`
  - `msix_queue_vector=0x....` (queue0)
- The selftest may also emit an informational standalone miniport IRQ line (useful for log scraping):
  - `virtio-blk-miniport-irq|INFO|mode=<intx|msi|unknown>|message_count=<n>|msix_config_vector=0x....|msix_queue0_vector=0x....`
- To make MSI/MSI-X a hard requirement in the in-tree QEMU harness:
  - Host:
    - request a larger MSI-X table size (best-effort): `-VirtioMsixVectors N` / `--virtio-msix-vectors N` (global) or
      `-VirtioBlkVectors N` / `--virtio-blk-vectors N` (virtio-blk only)
    - require MSI-X enabled (host-side check): `-RequireVirtioBlkMsix` / `--require-virtio-blk-msix`
  - Guest selftest: `--expect-blk-msi` (or `AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1`)
- See `../tests/guest-selftest/README.md` for how to build/run the tool.

See also: [`docs/windows/virtio-pci-modern-interrupt-debugging.md`](../../../docs/windows/virtio-pci-modern-interrupt-debugging.md).

## Files

- `src/aero_virtio_blk.c` – StorPort miniport driver implementation.
- `include/aero_virtio_blk.h` – driver-local definitions.
- `inf/aero_virtio_blk.inf` – storage class INF for installation on Win7 x86/x64.

## Building

### Supported: WDK10 / MSBuild (CI path)

CI builds this driver via the MSBuild project:

- `drivers/windows7/virtio-blk/aero_virtio_blk.vcxproj`

From a Windows host with the WDK installed:

```powershell
# From the repo root:
.\ci\install-wdk.ps1
.\ci\build-drivers.ps1 -ToolchainJson .\out\toolchain.json -Drivers windows7/virtio-blk
```

Build outputs are staged under:

- `out/drivers/windows7/virtio-blk/x86/aero_virtio_blk.sys`
- `out/drivers/windows7/virtio-blk/x64/aero_virtio_blk.sys`

To stage an installable/signable package, copy the built SYS into the package staging folder:

```text
drivers/windows7/virtio-blk/inf/aero_virtio_blk.sys
```

### Legacy/deprecated: WDK 7.1 `build.exe`

For local development you can also use the legacy WinDDK 7600 `build` utility (`sources`/`makefile` are kept for that workflow).

## Hardware IDs

The INF binds to the modern virtio-blk PCI ID:

- `PCI\VEN_1AF4&DEV_1042&REV_01` (modern-only virtio-blk; requires `disable-legacy=on` and `x-pci-revision=0x01`)

## Repo layout note (canonical driver)

This repository intentionally keeps **exactly one** `aero_virtio_blk` driver package that binds to
`PCI\VEN_1AF4&DEV_1042&REV_01` so CI builds and guest-tools packaging are deterministic.

The older duplicate under `drivers/win7/virtio-blk/` has been removed.

## Installation (non-boot disk)

1. Copy `inf/aero_virtio_blk.inf` and `aero_virtio_blk.sys` into the **same directory** on the guest.
2. In Device Manager, update the driver for the unknown storage controller and point it at the INF.
3. The disk should appear via `disk.sys` and be visible in Disk Management.

## Boot disk usage

The INF installs the service as `StartType = 0` and `LoadOrderGroup = "SCSI Miniport"` so it can be used as a boot-start storage driver when the system disk is exposed as virtio-blk.

For offline image integration, inject the driver into the Windows image and ensure the PCI hardware ID is present in the critical device database (integration handled by separate tooling/tasks).

## Diagnostics

The driver supports a minimal `IOCTL_SCSI_MINIPORT` query:

- `SRB_IO_CONTROL.Signature = "AEROVBLK"`
- `SRB_IO_CONTROL.ControlCode = 0x8000A001`

Returns `AEROVBLK_QUERY_INFO` (negotiated features + queue stats + interrupt mode/MSI-X vectors + abort/reset counters).

The output struct is **variable-length** for backwards compatibility: callers that only
understand the original v1 layout can request/consume just the first 16 bytes (through
`UsedIdx`).

For robustness testing, the driver also supports a reset trigger IOCTL:

- `SRB_IO_CONTROL.ControlCode = 0x8000A002` (force a device/queue reset)

This IOCTL is intended as a **debug-only stress hook** and returns `STATUS_NOT_SUPPORTED`
in free builds (unless the driver is compiled with `DBG=1`).
