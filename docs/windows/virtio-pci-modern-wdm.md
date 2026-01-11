# Virtio PCI modern transport bring-up (Windows 7, WDM + INTx)

This page describes the **WDM** (non-KMDF) bring-up flow for Aero **virtio-pci modern** devices (Virtio 1.0+, PCI vendor capabilities + MMIO).

It is written around the WDM-only helpers under `drivers/windows7/virtio/common/`:

- `include/virtio_pci_modern_wdm.h` + `src/virtio_pci_modern_wdm.c`
- `include/virtio_pci_intx_wdm.h` + `src/virtio_pci_intx_wdm.c`

For the binding transport contract, see: [`docs/windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md).

## Aero contract v1 transport expectations (device-model side)

Contract v1 (`AERO-W7-VIRTIO`, PCI Revision ID `0x01`) locks down the modern transport to keep Windows 7 bring-up deterministic.
The transport-relevant requirements are:

### BAR0 MMIO

- **BAR0** is a **memory BAR** (MMIO), little-endian, size **>= 0x4000**.
- All required virtio configuration windows are in **BAR0** (contract v1 fixed layout).

Contract v1 fixed layout (all in BAR0):

| Capability | `cfg_type` | Offset | Minimum length |
|---|---:|---:|---:|
| `COMMON_CFG` | 1 | `0x0000` | `0x0100` |
| `NOTIFY_CFG` | 2 | `0x1000` | `0x0100` |
| `ISR_CFG` | 3 | `0x2000` | `0x0020` |
| `DEVICE_CFG` | 4 | `0x3000` | `0x0100` |

`NOTIFY_CFG.notify_off_multiplier` is required to be `4` by contract v1.

### Required virtio vendor capabilities (PCI cap ID `0x09`)

PCI config space must contain a valid capability list with these virtio vendor-specific capabilities:

- `VIRTIO_PCI_CAP_COMMON_CFG` (`cfg_type = 1`) → `common_cfg`
- `VIRTIO_PCI_CAP_NOTIFY_CFG` (`cfg_type = 2`) → notify doorbell region + `notify_off_multiplier`
- `VIRTIO_PCI_CAP_ISR_CFG` (`cfg_type = 3`) → ISR status register (read-to-ack)
- `VIRTIO_PCI_CAP_DEVICE_CFG` (`cfg_type = 4`) → device-specific config window

### INTx + ISR read-to-ack semantics

Contract v1 requires **INTx**. The key behavior is:

- The device asserts INTx when it sets any ISR cause bit.
- The driver **must read** the ISR status byte to **acknowledge** the interrupt and deassert the line.
- The ISR byte is **read-to-ack**; reading returns the pending cause bits and clears them.
- If the ISR byte reads as `0`, the interrupt is **not** for this device (important for shared vectors).

The helper `virtio_pci_intx_wdm` implements the canonical Windows 7 INTx pattern: read ISR in the ISR (ack) and dispatch to driver callbacks at DPC level.

### Required feature bits

All Aero devices require:

- `VIRTIO_F_VERSION_1` (bit 32) — always enforced by `VirtioPciNegotiateFeatures`.
- `VIRTIO_F_RING_INDIRECT_DESC` (bit 28) — drivers should include this in their **Required** feature mask (contract v1 requires it for all devices).

## IRQL + locking rules (from SAL annotations)

### PASSIVE_LEVEL only (PnP/start/stop paths)

These helpers must be called at `PASSIVE_LEVEL`:

- `VirtioPciModernWdmInit`, `VirtioPciModernWdmMapBars`, `VirtioPciModernWdmUnmapBars`, `VirtioPciModernWdmUninit`
- `VirtioPciResetDevice`
- `VirtioPciNegotiateFeatures`
- `VirtioIntxConnect`, `VirtioIntxDisconnect`

In practice, they belong in `IRP_MN_START_DEVICE` / stop/remove handling, **not** in your ISR/DPC.

### <= DISPATCH_LEVEL (DPC-safe helpers)

These helpers are `<= DISPATCH_LEVEL` and are safe to call from a DPC:

- queue configuration/notification: `VirtioPciGetQueueSize`, `VirtioPciSetupQueue`, `VirtioPciDisableQueue`, `VirtioPciNotifyQueue`
- config window access: `VirtioPciReadDeviceConfig`, `VirtioPciWriteDeviceConfig`
- status bits: `VirtioPciAddStatus`, `VirtioPciGetStatus`, `VirtioPciFailDevice`

### CommonCfg selector serialization (required)

`common_cfg` contains device-global selector registers (`device_feature_select`, `driver_feature_select`, `queue_select`).
Any multi-step sequence that uses selectors must be serialized.

`virtio_pci_modern_wdm` provides a per-device spinlock and exposes it via:

- `VirtioPciCommonCfgAcquire` / `VirtioPciCommonCfgRelease`

Most helper APIs take this lock internally. You only need to call `VirtioPciCommonCfgAcquire/Release` if you:

- access `Dev->CommonCfg` fields directly, or
- need to batch multiple `common_cfg` operations into one atomic sequence.

## Canonical WDM PnP flow (modern transport + INTx)

### `IRP_MN_START_DEVICE`

High-level sequencing (omitting the IRP forwarding boilerplate):

1. **Query PCI interface + parse virtio caps**
   - `VirtioPciModernWdmInit(LowerDeviceObject, &ctx->Vdev)`
2. **Map BARs**
   - `VirtioPciModernWdmMapBars(&ctx->Vdev, rawList, translatedList)`
3. **Negotiate features**
   - `VirtioPciNegotiateFeatures(&ctx->Vdev, Required, Wanted, &Negotiated)`
4. **Allocate and program virtqueues**
   - allocate split-ring memory (DMA) for each queue
   - `VirtioPciSetupQueue(&ctx->Vdev, q, descPa, availPa, usedPa)`
5. **Connect INTx**
   - locate `CmResourceTypeInterrupt` in the translated resource list
   - `VirtioIntxConnect(DeviceObject, InterruptDescTranslated, ctx->Vdev.IsrStatus, ...)`
6. **Set `DRIVER_OK`**
   - `VirtioPciAddStatus(&ctx->Vdev, VIRTIO_STATUS_DRIVER_OK)`

### `IRP_MN_STOP_DEVICE` / `IRP_MN_REMOVE_DEVICE`

Typical safe teardown order:

1. `VirtioIntxDisconnect(&ctx->Intx)` (waits for in-flight DPC completion)
2. `VirtioPciResetDevice(&ctx->Vdev)`
3. Free queue/ring memory (driver-owned; out of scope for transport helpers)
4. `VirtioPciModernWdmUnmapBars(&ctx->Vdev)`
5. On final remove: `VirtioPciModernWdmUninit(&ctx->Vdev)`

## Minimal pseudo-code (WDM START/STOP with the helpers)

This snippet is intentionally simplified and omits DMA allocation details, queue draining, and IRP forwarding.

```c
#include "virtio_pci_modern_wdm.h"
#include "virtio_pci_intx_wdm.h"

/* Virtio ring feature bits are not currently centralized in a single header. */
#ifndef VIRTIO_F_RING_INDIRECT_DESC
#define VIRTIO_F_RING_INDIRECT_DESC (1ull << 28)
#endif

typedef struct _DEVICE_CONTEXT {
    PDEVICE_OBJECT Self;
    PDEVICE_OBJECT Lower; // attached device object

    VIRTIO_PCI_MODERN_WDM_DEVICE Vdev;
    VIRTIO_INTX_WDM Intx;

    // Driver-owned queue state + ring allocations live here.
} DEVICE_CONTEXT;

static VOID
EvtVirtioQueueDpc(_In_opt_ PVOID Context)
{
    DEVICE_CONTEXT *ctx = (DEVICE_CONTEXT *)Context;
    // DISPATCH_LEVEL: drain used rings, complete requests, etc.
}

static VOID
EvtVirtioConfigDpc(_In_opt_ PVOID Context)
{
    DEVICE_CONTEXT *ctx = (DEVICE_CONTEXT *)Context;
    // DISPATCH_LEVEL: re-read device config if your device supports changes.
}

static const CM_PARTIAL_RESOURCE_DESCRIPTOR *
FindTranslatedInterruptDesc(_In_ PCM_RESOURCE_LIST ResourcesTranslated)
{
    // Walk ResourcesTranslated to find the CmResourceTypeInterrupt descriptor.
    // (Implementation omitted.)
    return InterruptDescTranslated;
}

NTSTATUS
StartDevice(_Inout_ DEVICE_CONTEXT *ctx, _In_ PIRP Irp)
{
    NTSTATUS status;
    UINT64 negotiated = 0;

    PCM_RESOURCE_LIST raw = IoGetCurrentIrpStackLocation(Irp)->Parameters.StartDevice.AllocatedResources;
    PCM_RESOURCE_LIST translated =
        IoGetCurrentIrpStackLocation(Irp)->Parameters.StartDevice.AllocatedResourcesTranslated;

    status = VirtioPciModernWdmInit(ctx->Lower, &ctx->Vdev);
    if (!NT_SUCCESS(status)) return status;

    status = VirtioPciModernWdmMapBars(&ctx->Vdev, raw, translated);
    if (!NT_SUCCESS(status)) goto fail_uninit;

    // Contract v1 baseline:
    const UINT64 required = VIRTIO_F_RING_INDIRECT_DESC;
    const UINT64 wanted = 0;

    status = VirtioPciNegotiateFeatures(&ctx->Vdev, required, wanted, &negotiated);
    if (!NT_SUCCESS(status)) goto fail_unmap;

    // Allocate + program queues (queue memory allocation omitted).
    ULONGLONG descPa = /* DMA address of descriptor table */;
    ULONGLONG availPa = /* DMA address of avail ring */;
    ULONGLONG usedPa = /* DMA address of used ring */;

    status = VirtioPciSetupQueue(&ctx->Vdev, /*QueueIndex=*/0, descPa, availPa, usedPa);
    if (!NT_SUCCESS(status)) goto fail_reset;

    // Notify after publishing avail entries (shown here for completeness).
    VirtioPciNotifyQueue(&ctx->Vdev, /*QueueIndex=*/0);

    const CM_PARTIAL_RESOURCE_DESCRIPTOR *intDesc = FindTranslatedInterruptDesc(translated);
    status = VirtioIntxConnect(ctx->Self,
                               intDesc,
                               ctx->Vdev.IsrStatus,
                               EvtVirtioQueueDpc,
                               ctx,
                               EvtVirtioConfigDpc,
                               ctx,
                               &ctx->Intx);
    if (!NT_SUCCESS(status)) goto fail_reset;

    VirtioPciAddStatus(&ctx->Vdev, VIRTIO_STATUS_DRIVER_OK);
    return STATUS_SUCCESS;

fail_reset:
    VirtioPciResetDevice(&ctx->Vdev);
fail_unmap:
    VirtioPciModernWdmUnmapBars(&ctx->Vdev);
fail_uninit:
    VirtioPciModernWdmUninit(&ctx->Vdev);
    return status;
}

VOID
StopDevice(_Inout_ DEVICE_CONTEXT *ctx)
{
    VirtioIntxDisconnect(&ctx->Intx);
    VirtioPciResetDevice(&ctx->Vdev);
    VirtioPciModernWdmUnmapBars(&ctx->Vdev);
    // Keep ctx->Vdev initialized across STOP/START; call Uninit only on REMOVE.
}
```

## Debugging notes: what the helpers do internally

The helpers are thin wrappers over the standard WDM patterns, which is useful to know when debugging bring-up:

- `VirtioPciModernWdmInit`:
  - sends `IRP_MN_QUERY_INTERFACE` for `GUID_PCI_BUS_INTERFACE_STANDARD` to the lower stack
  - reads PCI Revision ID and enforces contract v1 (`0x01`)
  - reads PCI BAR programming and parses the virtio vendor capability list using
    `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`
  - enforces contract v1 placement (all required virtio caps in BAR0)

- `VirtioPciModernWdmMapBars`:
  - matches BAR memory resources from `IRP_MN_START_DEVICE`’s `CM_RESOURCE_LIST`
  - maps MMIO with `MmMapIoSpace(MmNonCached)`
  - populates `Dev->CommonCfg`, `Dev->NotifyBase`, `Dev->IsrStatus`, `Dev->DeviceCfg`,
    plus `Dev->NotifyOffMultiplier`

- `VirtioIntxConnect`:
  - connects an INTx ISR via `IoConnectInterrupt` using the *translated* interrupt resource
  - the ISR performs the required virtio read-to-ack by reading `Dev->IsrStatus`
  - work is dispatched to optional per-device callbacks from a DPC; disconnect waits for in-flight DPC completion

## How this relates to other virtio code in this repo

### `drivers/windows7/virtio-modern/common/` (fixed-layout helpers)

`drivers/windows7/virtio-modern/common/` provides contract v1 helpers that assume:

- BAR0 is **already mapped** by the caller, and
- the fixed Aero MMIO layout is in use.

This is useful in environments that don’t have WDM PnP start IRPs/resources (e.g. StorPort/NDIS).

### `drivers/win7/virtio/virtio-core/` (KMDF-centric modern transport)

`virtio-core` provides a more general virtio-pci modern discovery/mapping layer, primarily targeted at **KMDF** drivers.

It can be built without WDF (`VIRTIO_CORE_USE_WDF=0`), but it exports several transport helper symbols
(`VirtioPciResetDevice`, `VirtioPciNegotiateFeatures`, etc.) that overlap with `virtio_pci_modern_wdm`.
Don’t link both into the same driver without resolving the symbol/name conflicts.
