# Virtio PCI modern transport bring-up (Windows 7, WDM + INTx)

This page describes the **WDM** (non-KMDF) bring-up flow for Aero
**virtio-pci modern** devices (Virtio 1.0+, PCI vendor capabilities + MMIO).

For WDM (non-KMDF) Windows 7 virtio drivers in this repo (for example
`virtio-snd`), the canonical transport implementation is:

- `drivers/windows/virtio/pci-modern/` (`VirtioPciModernTransport*`)

Note: Windows 7 **miniport** drivers (`virtio-blk`, `virtio-net`) use the
miniport-friendly shim under `drivers/windows7/virtio/common/` instead.

It is implemented in:

- `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.h`
- `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.c`

For the shared Windows 7 **INTx** helper (ISR read-to-ack + DPC dispatch), use:

- `drivers/windows7/virtio/common/include/virtio_pci_intx_wdm.h`
- `drivers/windows7/virtio/common/src/virtio_pci_intx_wdm.c`

For the binding device/driver contract, see:
[`docs/windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md).

## Aero contract v1 transport expectations (device-model side)

Contract v1 (`AERO-W7-VIRTIO`, PCI Revision ID `0x01`) locks down the modern
transport to keep Windows 7 bring-up deterministic.

### BAR0 MMIO

- **BAR0** is a **memory BAR** (MMIO), little-endian, size **>= 0x4000**.
- All required virtio configuration windows are in **BAR0** (contract v1 fixed
  layout).

Contract v1 fixed layout (all in BAR0):

| Capability | `cfg_type` | Offset | Minimum length |
|---|---:|---:|---:|
| `COMMON_CFG` | 1 | `0x0000` | `0x0100` |
| `NOTIFY_CFG` | 2 | `0x1000` | `0x0100` |
| `ISR_CFG` | 3 | `0x2000` | `0x0020` |
| `DEVICE_CFG` | 4 | `0x3000` | `0x0100` |

`NOTIFY_CFG.notify_off_multiplier` is required to be `4` by contract v1.

### Required virtio vendor capabilities (PCI cap ID `0x09`)

PCI config space must contain a valid capability list with these virtio
vendor-specific capabilities:

- `VIRTIO_PCI_CAP_COMMON_CFG` (`cfg_type = 1`) → `common_cfg`
- `VIRTIO_PCI_CAP_NOTIFY_CFG` (`cfg_type = 2`) → notify doorbell region +
  `notify_off_multiplier`
- `VIRTIO_PCI_CAP_ISR_CFG` (`cfg_type = 3`) → ISR status register (read-to-ack)
- `VIRTIO_PCI_CAP_DEVICE_CFG` (`cfg_type = 4`) → device-specific config window

### INTx + ISR read-to-ack semantics

Contract v1 requires **INTx**:

- The device asserts INTx when it sets any ISR cause bit.
- The driver **must read** the ISR status byte to **acknowledge** the interrupt
  and deassert the line.
- The ISR byte is **read-to-ack**; reading returns the pending cause bits and
  clears them.
- If the ISR byte reads as `0`, the interrupt is **not** for this device
  (important for shared vectors).

The helper `virtio_pci_intx_wdm` implements the canonical Windows 7 INTx pattern:
read ISR in the ISR (ack) and dispatch to driver callbacks at DPC level.

### Required feature bits

All Aero modern devices require:

- `VIRTIO_F_VERSION_1` (bit 32) — always enforced by
  `VirtioPciModernTransportNegotiateFeatures`.
- `VIRTIO_F_RING_INDIRECT_DESC` (bit 28) — required by contract v1; strict-mode
  transport init/negotiation rejects devices that do not offer it.

## IRQL + locking notes

In practice:

- `VirtioPciModernTransportInit/Uninit` belong in `IRP_MN_START_DEVICE` and
  stop/remove handling and should be called at `PASSIVE_LEVEL` (they map/unmap
  MMIO and may query PCI config).
- `VirtioPciModernTransportNegotiateFeatures` should be called at
  `PASSIVE_LEVEL` (it performs a device reset handshake and may sleep/yield).
- Status/queue/config helpers are designed to be usable at `<= DISPATCH_LEVEL`
  (for example from a DPC), but queue programming is typically performed during
  start at `PASSIVE_LEVEL`.

Note: the transport reset helper is IRQL-aware. In kernel-mode builds it will
avoid long stalls at elevated IRQL by capping the busy-wait budget and returning
even if the device does not complete the reset handshake within that budget.

`common_cfg` contains selector registers (`device_feature_select`,
`driver_feature_select`, `queue_select`). The transport creates a per-device
spinlock (provided by the OS interface) and uses it internally to serialize
selector-based sequences.

## Canonical WDM PnP flow (modern transport + INTx)

High-level sequencing for `IRP_MN_START_DEVICE` (omitting IRP forwarding boilerplate):

1. **Query bus interface** (e.g. `BUS_INTERFACE_STANDARD`) and read PCI config
2. **Locate BAR0** in the translated resource list and pass its translated
    physical address/length to `VirtioPciModernTransportInit`
3. **Negotiate features**
4. **Allocate and program virtqueues**
5. **Connect INTx** using `VirtioIntxConnect` (from `virtio_pci_intx_wdm`)
6. **Set `DRIVER_OK`**

For stop/remove, disconnect INTx first, reset the device, free queue resources,
then uninit the transport.

## Minimal pseudo-code (WDM START/STOP with the transport + INTx helper)

This snippet is intentionally simplified and omits DMA allocation details,
queue draining, and IRP forwarding.

```c
#include "virtio_pci_intx_wdm.h"
#include "virtio_pci_modern_transport.h"

typedef struct _DEVICE_CONTEXT {
    PDEVICE_OBJECT Self;
    PDEVICE_OBJECT Lower;

    VIRTIO_PCI_MODERN_OS_INTERFACE Os;
    VIRTIO_PCI_MODERN_TRANSPORT Pci;

    VIRTIO_INTX Intx;
} DEVICE_CONTEXT;

NTSTATUS StartDevice(_Inout_ DEVICE_CONTEXT *ctx,
                     _In_ PCM_RESOURCE_LIST raw,
                     _In_ PCM_RESOURCE_LIST translated)
{
    // 1) Fill ctx->Os callbacks:
    //    - PciRead8/16/32 via BUS_INTERFACE_STANDARD
    //    - MapMmio via MmMapIoSpace
    //    - StallUs via KeStallExecutionProcessor
    //    - SpinlockCreate/Acquire/Release via Ke* spinlocks
    //
    // 2) Find BAR0 translated physical address/length from CM_RESOURCE_LIST.
    UINT64 bar0_pa = /* ... */;
    UINT32 bar0_len = /* ... */;

    NTSTATUS st = VirtioPciModernTransportInit(&ctx->Pci,
                                              &ctx->Os,
                                              VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT,
                                              bar0_pa,
                                              bar0_len);
    if (!NT_SUCCESS(st)) return st;

    UINT64 negotiated = 0;
    st = VirtioPciModernTransportNegotiateFeatures(&ctx->Pci,
                                                   /*Required=*/((UINT64)1u << 28), // INDIRECT_DESC
                                                   /*Wanted=*/0,
                                                   &negotiated);
    if (!NT_SUCCESS(st)) goto fail_uninit;

    // 3) Allocate and program queues...
    st = VirtioPciModernTransportSetupQueue(&ctx->Pci, /*q=*/0, /*desc_pa=*/0, /*avail_pa=*/0, /*used_pa=*/0);
    if (!NT_SUCCESS(st)) goto fail_reset;

    // 4) Connect INTx (translated interrupt resource + ctx->Pci.IsrStatus).
    st = VirtioIntxConnect(ctx->Self,
                           /*InterruptDescTranslated=*/NULL,
                           ctx->Pci.IsrStatus,
                           /*EvtConfigChange=*/NULL,
                           /*EvtQueueWork=*/NULL,
                           /*EvtDpc=*/NULL,
                           /*Cookie=*/ctx,
                           &ctx->Intx);
    if (!NT_SUCCESS(st)) goto fail_reset;

    VirtioPciModernTransportAddStatus(&ctx->Pci, VIRTIO_STATUS_DRIVER_OK);
    return STATUS_SUCCESS;

fail_reset:
    VirtioPciModernTransportResetDevice(&ctx->Pci);
fail_uninit:
    VirtioPciModernTransportUninit(&ctx->Pci);
    return st;
}

VOID StopDevice(_Inout_ DEVICE_CONTEXT *ctx)
{
    VirtioIntxDisconnect(&ctx->Intx);
    VirtioPciModernTransportResetDevice(&ctx->Pci);
    VirtioPciModernTransportUninit(&ctx->Pci);
}
```

Note: in strict mode `VirtioPciModernTransportInit` verifies that the BAR0
physical address passed to it matches the BAR0 base programmed in PCI config
space. If your driver stack reports different *raw* vs *translated* addresses,
pass the BAR0 bus address and translate inside your `MapMmio` callback.

## How this relates to other virtio code in this repo

### `drivers/windows/virtio/pci-modern/` (portable modern transport)

`drivers/windows/virtio/pci-modern/` provides the canonical WDF-free virtio-pci modern transport used by Aero’s
Windows 7 **WDM** virtio drivers (for example `virtio-snd`). It:

- parses the PCI vendor capability list (contract §1.3),
- validates `RevisionID == 0x01`, and
- supports **STRICT** (enforce fixed offsets) vs **COMPAT** (accept relocated caps / 32-bit BAR0) policy.

Drivers integrate it by implementing the `VIRTIO_PCI_MODERN_OS_INTERFACE` callbacks (PCI config reads, BAR mapping,
stall, and selector-serialization lock).

### `drivers/windows7/virtio/common/` (miniport-friendly Win7 helpers)

Windows 7 **miniport** drivers (`virtio-blk` / `virtio-net`) use the miniport-friendly modern transport shim in:

- `drivers/windows7/virtio/common/include/virtio_pci_modern_miniport.h`
- `drivers/windows7/virtio/common/src/virtio_pci_modern_miniport.c`

This shim is shaped for NDIS/StorPort: callers provide a mapped BAR0 pointer and a cached 256-byte PCI config snapshot,
so drivers do not need to implement `VIRTIO_PCI_MODERN_OS_INTERFACE`.

### `drivers/win7/virtio/virtio-core/` (KMDF-centric modern transport)

`virtio-core` provides a more general virtio-pci modern discovery/mapping layer, primarily targeted at **KMDF** drivers.

It can be built without WDF (`VIRTIO_CORE_USE_WDF=0`) but uses a different device
abstraction (`VIRTIO_PCI_MODERN_DEVICE`) and provides its own init/mapping
helpers. For WDF-free drivers in this repo:

- WDM drivers typically use `drivers/windows/virtio/pci-modern/`.
- Miniport drivers typically use `drivers/windows7/virtio/common/`.
