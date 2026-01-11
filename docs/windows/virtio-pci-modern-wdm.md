# Virtio PCI modern transport bring-up (Windows 7, WDM + INTx)

This page describes the **WDM** (non-KMDF) bring-up flow for Aero **virtio-pci modern** devices (Virtio 1.0+, PCI vendor capabilities + MMIO).

It is written around the canonical, WDF-free virtio-pci modern transport:

- `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.h`
- `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.c`

and the reusable WDM INTx helper:

- `drivers/windows7/virtio/common/include/virtio_pci_intx_wdm.h`
- `drivers/windows7/virtio/common/src/virtio_pci_intx_wdm.c`

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

- `VIRTIO_F_VERSION_1` (bit 32) — always enforced by `VirtioPciModernTransportNegotiateFeatures`.
- `VIRTIO_F_RING_INDIRECT_DESC` (bit 28) — drivers should include this in their **Required** feature mask (contract v1 requires it for all devices).

## IRQL + locking rules (from SAL annotations)

### PASSIVE_LEVEL only (PnP/start/stop paths)

These helpers must be called at `PASSIVE_LEVEL`:

- `VirtioPciModernTransportInit`, `VirtioPciModernTransportUninit`
- `VirtioPciModernTransportResetDevice`
- `VirtioPciModernTransportNegotiateFeatures`
- `VirtioIntxConnect`, `VirtioIntxDisconnect`

In practice, they belong in `IRP_MN_START_DEVICE` / stop/remove handling, **not** in your ISR/DPC.

### <= DISPATCH_LEVEL (DPC-safe helpers)

These helpers are `<= DISPATCH_LEVEL` and are safe to call from a DPC:

- queue configuration/notification: `VirtioPciModernTransportGetQueueSize`, `VirtioPciModernTransportSetupQueue`,
  `VirtioPciModernTransportDisableQueue`, `VirtioPciModernTransportNotifyQueue`
- config window access: `VirtioPciModernTransportReadDeviceConfig`, `VirtioPciModernTransportWriteDeviceConfig`
- status bits: `VirtioPciModernTransportAddStatus`, `VirtioPciModernTransportGetStatus`
- INTx ACK helper: `VirtioPciModernTransportReadIsrStatus`

### CommonCfg selector serialization (required)

`common_cfg` contains device-global selector registers (`device_feature_select`, `driver_feature_select`, `queue_select`).
Any multi-step sequence that uses selectors must be serialized.

`VirtioPciModernTransport*` takes a lock internally for all selector-based operations.
Drivers should prefer the helper APIs over direct `CommonCfg` accesses.

## Canonical WDM PnP flow (modern transport + INTx)

### `IRP_MN_START_DEVICE`

High-level sequencing (omitting the IRP forwarding boilerplate):

1. **Read a 256-byte PCI config snapshot + locate BAR0**
   - acquire `PCI_BUS_INTERFACE_STANDARD`
   - read config space (offset 0, length 256)
   - locate the translated BAR0 `CmResourceTypeMemory` range from the `CM_RESOURCE_LIST`
2. **Initialize the canonical transport (PCI cap parsing + BAR0 mapping)**
   - implement the required `VIRTIO_PCI_MODERN_OS_INTERFACE` callbacks (PCI reads, BAR0 MMIO mapping, stall, lock)
   - `VirtioPciModernTransportInit(&ctx->Transport, &ctx->TransportOs, STRICT, bar0_pa, bar0_len)`
3. **Negotiate features**
   - `VirtioPciModernTransportNegotiateFeatures(&ctx->Transport, Required, Wanted, &Negotiated)`
4. **Allocate and program virtqueues**
    - allocate split-ring memory (DMA) for each queue
    - `VirtioPciModernTransportSetupQueue(&ctx->Transport, q, descPa, availPa, usedPa)`
5. **Connect INTx**
    - locate `CmResourceTypeInterrupt` in the translated resource list
    - `VirtioIntxConnect(DeviceObject, InterruptDescTranslated, ctx->Transport.IsrStatus, EvtConfigChange, EvtQueueWork, EvtDpc, Cookie, &ctx->Intx)`
6. **Set `DRIVER_OK`**
    - `VirtioPciModernTransportAddStatus(&ctx->Transport, VIRTIO_STATUS_DRIVER_OK)`

### `IRP_MN_STOP_DEVICE` / `IRP_MN_REMOVE_DEVICE`

Typical safe teardown order:

1. `VirtioIntxDisconnect(&ctx->Intx)` (waits for in-flight DPC completion)
2. `VirtioPciModernTransportResetDevice(&ctx->Transport)`
3. Free queue/ring memory (driver-owned; out of scope for transport helpers)
4. `VirtioPciModernTransportUninit(&ctx->Transport)`

## Minimal pseudo-code (WDM START/STOP with the helpers)

This snippet is intentionally simplified and omits DMA allocation details, queue draining, and IRP forwarding.

```c
#include "virtio_pci_intx_wdm.h"
#include "virtio_pci_modern_transport.h"

  typedef struct _DEVICE_CONTEXT {
      PDEVICE_OBJECT Self;
      PDEVICE_OBJECT Lower; // attached device object

      PCI_BUS_INTERFACE_STANDARD Pci;
      BOOLEAN PciAcquired;
      UCHAR PciCfg[256];

      VIRTIO_PCI_MODERN_TRANSPORT Transport;
      VIRTIO_PCI_MODERN_OS_INTERFACE TransportOs;
      VIRTIO_INTX Intx;
  
      // Driver-owned queue state + ring allocations live here.
  } DEVICE_CONTEXT;
 
 static VOID
 EvtVirtioQueueWork(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
 {
     DEVICE_CONTEXT *ctx = (DEVICE_CONTEXT *)Cookie;
     UNREFERENCED_PARAMETER(Intx);
     // DISPATCH_LEVEL: drain used rings, complete requests, etc.
 }
 
 static VOID
 EvtVirtioConfigChange(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
 {
     DEVICE_CONTEXT *ctx = (DEVICE_CONTEXT *)Cookie;
     UNREFERENCED_PARAMETER(Intx);
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
     PHYSICAL_ADDRESS bar0Pa = {0};
     UINT32 bar0Len = 0;
 
     PCM_RESOURCE_LIST raw = IoGetCurrentIrpStackLocation(Irp)->Parameters.StartDevice.AllocatedResources;
     PCM_RESOURCE_LIST translated =
         IoGetCurrentIrpStackLocation(Irp)->Parameters.StartDevice.AllocatedResourcesTranslated;
 
     // Acquire PCI interface + read PCI config snapshot (256 bytes) into ctx->PciCfg.
     // Locate BAR0 memory range (bar0Pa/bar0Len) from CM_RESOURCE_LIST.
     // (Implementation omitted.)

     // Initialize ctx->TransportOs with PCI/MMIO/locking callbacks.
     // (Implementation omitted.)

     status = VirtioPciModernTransportInit(
         &ctx->Transport,
         &ctx->TransportOs,
         VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT,
         (UINT64)bar0Pa.QuadPart,
         bar0Len);
     if (!NT_SUCCESS(status)) return status;

     // Contract v1 baseline:
     const UINT64 required = (UINT64)VIRTIO_RING_F_INDIRECT_DESC;
     const UINT64 wanted = 0;
 
     status = VirtioPciModernTransportNegotiateFeatures(&ctx->Transport, required, wanted, &negotiated);
     if (!NT_SUCCESS(status)) goto fail_unmap;
 
     // Allocate + program queues (queue memory allocation omitted).
     ULONGLONG descPa = /* DMA address of descriptor table */;
     ULONGLONG availPa = /* DMA address of avail ring */;
     ULONGLONG usedPa = /* DMA address of used ring */;
 
     status = VirtioPciModernTransportSetupQueue(&ctx->Transport, /*QueueIndex=*/0, descPa, availPa, usedPa);
     if (!NT_SUCCESS(status)) goto fail_reset;
 
     // Notify after publishing avail entries (shown here for completeness).
     VirtioPciModernTransportNotifyQueue(&ctx->Transport, /*QueueIndex=*/0);
 
      const CM_PARTIAL_RESOURCE_DESCRIPTOR *intDesc = FindTranslatedInterruptDesc(translated);
      status = VirtioIntxConnect(ctx->Self,
                                 intDesc,
                                 ctx->Transport.IsrStatus,
                                 EvtVirtioConfigChange,
                                 EvtVirtioQueueWork,
                                 /*EvtDpc=*/NULL,
                                 /*Cookie=*/ctx,
                                 &ctx->Intx);
      if (!NT_SUCCESS(status)) goto fail_reset;
 
     VirtioPciModernTransportAddStatus(&ctx->Transport, VIRTIO_STATUS_DRIVER_OK);
     return STATUS_SUCCESS;
 
 fail_reset:
     VirtioPciModernTransportResetDevice(&ctx->Transport);
 fail_unmap:
     VirtioPciModernTransportUninit(&ctx->Transport);
     return status;
 }
 
 VOID
 StopDevice(_Inout_ DEVICE_CONTEXT *ctx)
 {
     VirtioIntxDisconnect(&ctx->Intx);
     VirtioPciModernTransportResetDevice(&ctx->Transport);
     VirtioPciModernTransportUninit(&ctx->Transport);
 }
```

## Debugging notes: what the helpers do internally

The canonical `VirtioPciModernTransport*` helpers are thin wrappers over the standard WDM patterns, which is useful to know when debugging bring-up:

- `VirtioPciModernTransportInit`:
  - reads PCI identity (Vendor/Device/Revision/Subsys/InterruptPin) via the OS callbacks and enforces contract v1 (`REV_01`)
  - parses the virtio vendor capability list using `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`
  - maps BAR0 via the OS callbacks and populates:
    - `Transport.CommonCfg`, `Transport.NotifyBase`, `Transport.IsrStatus`, `Transport.DeviceCfg`
    - `Transport.NotifyOffMultiplier`
  - exposes diagnostics for init failures via `Transport.InitError` and `Transport.CapParseResult`

- `VirtioPciModernTransportNegotiateFeatures`:
  - performs the virtio status handshake (RESET → ACKNOWLEDGE → DRIVER → FEATURES_OK)
  - always requires `VIRTIO_F_VERSION_1`
  - in STRICT mode, enforces the contract-v1 ring feature policy (requires INDIRECT_DESC; forbids EVENT_IDX/PACKED)

- `VirtioIntxConnect`:
  - connects an INTx ISR via `IoConnectInterrupt` using the *translated* interrupt resource
  - the ISR performs the required virtio read-to-ack by reading the ISR status byte
  - work is dispatched to optional per-device callbacks from a DPC; disconnect waits for in-flight DPC completion

## How this relates to other virtio code in this repo

### `drivers/windows/virtio/pci-modern/` (portable modern transport)

`drivers/windows/virtio/pci-modern/` provides the canonical WDF-free virtio-pci modern transport used by Aero’s
Windows 7 drivers (StorPort/NDIS/WDM). It:

- parses the PCI vendor capability list (contract §1.3),
- validates `RevisionID == 0x01`, and
- supports **STRICT** (enforce fixed offsets) vs **COMPAT** (accept relocated caps / 32-bit BAR0) policy.

StorPort/NDIS drivers provide the required OS callbacks for PCI config access and BAR0 mapping.

### `drivers/win7/virtio/virtio-core/` (KMDF-centric modern transport)

`virtio-core` provides a more general virtio-pci modern discovery/mapping layer, primarily targeted at **KMDF** drivers.

It can be built without WDF (`VIRTIO_CORE_USE_WDF=0`), but it exports several transport helper symbols
(`VirtioPciResetDevice`, `VirtioPciNegotiateFeatures`, etc.) that overlap with `virtio_pci_modern_wdm`.
Don’t link both into the same driver without resolving the symbol/name conflicts.
