# Virtio PCI modern transport bring-up on Windows 7 (WDM + INTx)

This document is a practical bring-up guide for a **WDM (non-KMDF)** PnP function driver that speaks the **virtio-pci modern** (Virtio 1.0+) transport on Windows 7.

It is intentionally written around the **WDM** lifecycle (`IRP_MJ_PNP` + `IRP_MN_START_DEVICE`) and the Aero contract v1 assumptions:

- **BAR0 MMIO only** (no legacy I/O port transport).
- Virtio vendor capabilities provide **COMMON/NOTIFY/ISR/DEVICE** regions.
- **INTx is required**; **MSI-X is optional**. (`docs/windows7-virtio-driver-contract.md`)

Related docs / code:

- [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) (Aero contract v1: fixed BAR0 + fixed cap layout, INTx required)
- [`../virtio/virtqueue-split-ring-win7.md`](../virtio/virtqueue-split-ring-win7.md) (split virtqueue implementation notes)
- Portable virtio capability parser:
  - `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.{h,c}`
- Reference implementations in this repo (WDM-only):
  - `drivers/windows7/virtio-snd/include/pci_interface.h` (`VirtIoSndAcquirePciBusInterface`)
  - `drivers/windows7/virtio-snd/include/virtio_pci_modern_wdm.h` (`VIRTIOSND_TRANSPORT`, `VirtIoSndTransport*`)
  - `drivers/windows7/virtio-snd/include/virtiosnd_intx.h` (`VirtIoSndIntx*`)

---

## 0) IRQL / execution model (why the sequence matters)

- `IRP_MN_START_DEVICE` runs at **PASSIVE_LEVEL**. Do all slow work here:
  - query interfaces, read PCI config space, parse caps, map BARs (`MmMapIoSpace`), negotiate features, allocate queue memory, connect interrupts.
- INTx ISR runs at the device’s **DIRQL**. Do the absolute minimum:
  - **read the virtio ISR status byte** (read-to-clear) and queue a DPC.
- DPC runs at **DISPATCH_LEVEL**. Drain queues and do any MMIO sequences that need spin locks.

Keep all MMIO pointers in **nonpaged** storage and treat them as `volatile`.

---

## 1) Query `PCI_BUS_INTERFACE_STANDARD` (WDM: `IRP_MN_QUERY_INTERFACE`)

Virtio PCI bring-up needs PCI config space reads to:

- find BAR base addresses programmed by the bus,
- walk the PCI capability list and find virtio vendor-specific capabilities.

In WDM, you typically query `GUID_PCI_BUS_INTERFACE_STANDARD` from the lower (PCI) driver.

Minimal pattern (pseudo-code):

```c
static NTSTATUS QueryIfcCompletion(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PVOID Context)
{
    UNREFERENCED_PARAMETER(DeviceObject);
    UNREFERENCED_PARAMETER(Irp);
    KeSetEvent((PKEVENT)Context, IO_NO_INCREMENT, FALSE);
    return STATUS_MORE_PROCESSING_REQUIRED; // caller frees the IRP
}

static NTSTATUS QueryPciBusInterface(
    _In_ PDEVICE_OBJECT Lower,
    _Out_ PCI_BUS_INTERFACE_STANDARD* PciIfcOut
    )
{
    KEVENT event;
    PIRP irp;
    NTSTATUS status;

    KeInitializeEvent(&event, NotificationEvent, FALSE);
    RtlZeroMemory(PciIfcOut, sizeof(*PciIfcOut));

    irp = IoAllocateIrp((CCHAR)Lower->StackSize, FALSE);
    if (!irp) return STATUS_INSUFFICIENT_RESOURCES;

    irp->IoStatus.Status = STATUS_NOT_SUPPORTED;
    irp->IoStatus.Information = 0;

    IoSetCompletionRoutine(irp, QueryIfcCompletion, &event, TRUE, TRUE, TRUE);

    {
        PIO_STACK_LOCATION s = IoGetNextIrpStackLocation(irp);
        s->MajorFunction = IRP_MJ_PNP;
        s->MinorFunction = IRP_MN_QUERY_INTERFACE;
        s->Parameters.QueryInterface.InterfaceType = (LPGUID)&GUID_PCI_BUS_INTERFACE_STANDARD;
        s->Parameters.QueryInterface.Size = sizeof(*PciIfcOut);
        s->Parameters.QueryInterface.Version = PCI_BUS_INTERFACE_STANDARD_VERSION;
        s->Parameters.QueryInterface.Interface = (PINTERFACE)PciIfcOut;
        s->Parameters.QueryInterface.InterfaceSpecificData = NULL;
    }

    status = IoCallDriver(Lower, irp);
    if (status == STATUS_PENDING) {
        KeWaitForSingleObject(&event, Executive, KernelMode, FALSE, NULL);
        status = irp->IoStatus.Status;
    }

    IoFreeIrp(irp);

    if (!NT_SUCCESS(status)) return status;

    // Hold a reference for as long as you use the function pointers.
    if (PciIfcOut->InterfaceReference) {
        PciIfcOut->InterfaceReference(PciIfcOut->Context);
    }
    return STATUS_SUCCESS;
}
```

In this repo’s WDM virtio-snd driver, this step is wrapped by:

- `VirtIoSndAcquirePciBusInterface` / `VirtIoSndReleasePciBusInterface` (`drivers/windows7/virtio-snd/include/pci_interface.h`)

Release the interface on STOP/REMOVE:

```c
if (dx->PciIfc.InterfaceDereference) {
    dx->PciIfc.InterfaceDereference(dx->PciIfc.Context);
}
RtlZeroMemory(&dx->PciIfc, sizeof(dx->PciIfc));
```

---

## 2) Read BAR base addresses from config space (handle 64-bit BARs)

The virtio vendor capability parser needs the *actual* BAR base addresses (bus addresses) so it can compute `addr = bar_base + offset`.

Read the Type 0 header BAR registers at config offsets `0x10..0x24` (`6 * u32`). Then decode each BAR:

- If bit 0 is set, it’s an **I/O BAR** (should not be used for virtio modern in Aero).
- If memory BAR:
  - bits 2:1 = `0b10` means **64-bit BAR** and consumes two BAR slots (`BARn` + `BARn+1`).
  - mask low bits (`& ~0xFu`) to get the aligned base.

Pseudo-code:

```c
static NTSTATUS ReadBarBases(
    _In_ PCI_BUS_INTERFACE_STANDARD* Pci,
    _Out_writes_(6) ULONGLONG BarBase
    )
{
    ULONG barReg[6];
    ULONG n;

    RtlZeroMemory(BarBase, sizeof(ULONGLONG) * 6);

    n = Pci->ReadConfig(Pci->Context, 0 /*PCI_WHICHSPACE_CONFIG*/, barReg, 0x10, sizeof(barReg));
    if (n != sizeof(barReg)) return STATUS_DEVICE_DATA_ERROR;

    for (ULONG i = 0; i < 6; i++) {
        ULONG v = barReg[i];
        if (v == 0) continue;

        if (v & 0x1) {
            // IO BAR - ignore for virtio modern.
            continue;
        }

        ULONG memType = (v >> 1) & 0x3;
        if (memType == 0x2) { // 64-bit
            if (i == 5) return STATUS_DEVICE_CONFIGURATION_ERROR;
            BarBase[i] = ((ULONGLONG)barReg[i + 1] << 32) | (ULONGLONG)(v & ~0xFu);
            i++; // consume upper half slot
        } else {
            BarBase[i] = (ULONGLONG)(v & ~0xFu);
        }
    }

    return STATUS_SUCCESS;
}
```

---

## 3) Parse virtio vendor capabilities (COMMON/NOTIFY/ISR/DEVICE)

Read the first 256 bytes of PCI config space (enough for the standard cap list) and feed it into the portable parser:

```c
#include "virtio_pci_cap_parser.h"

uint8_t cfg[256];
uint64_t bar_addrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
virtio_pci_parsed_caps_t caps;

Pci->ReadConfig(Pci->Context, 0, cfg, 0, sizeof(cfg));

for (i = 0; i < VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT; i++) {
    bar_addrs[i] = (uint64_t)BarBase[i];
}

virtio_pci_cap_parse_result_t r = virtio_pci_cap_parse(cfg, sizeof(cfg), bar_addrs, &caps);
if (r != VIRTIO_PCI_CAP_PARSE_OK) {
    // unsupported / malformed device
}
```

The parser guarantees the required modern virtio regions exist and returns:

- `caps.common_cfg` (cfg_type 1)
- `caps.notify_cfg` (cfg_type 2) + `caps.notify_off_multiplier`
- `caps.isr_cfg` (cfg_type 3)
- `caps.device_cfg` (cfg_type 4)

> Aero contract v1 fixes these to BAR0 with offsets `{0x0000,0x1000,0x2000,0x3000}` respectively, but drivers should still parse/validate the capability list so bugs are caught early.

In this repo, virtio-snd stores the parsed results in `VIRTIOSND_TRANSPORT.Caps` and exposes the resolved MMIO pointers
(`CommonCfg`, `NotifyBase`, `IsrStatus`, `DeviceCfg`) after `VirtIoSndTransportInit()`.

---

## 4) Map BAR0 from translated CM resources and compute cap VAs

In `IRP_MN_START_DEVICE`, Windows provides:

- `AllocatedResources` (raw bus addresses)
- `AllocatedResourcesTranslated` (system physical addresses you can map)

You typically:

1. Find the `CmResourceTypeMemory` descriptor that corresponds to BAR0.
   - Aero contract v1 uses **exactly one MMIO BAR** so this is usually “the only memory resource”.
   - More defensively: match the raw resource `Start` against the BAR base you read from config space.
2. Map the **translated** address with `MmMapIoSpace`.
3. Compute each capability VA as `bar0_va + cap.offset`.

Pseudo-code:

```c
// PASSIVE_LEVEL only.
dx->Bar0Length = memDesc->u.Memory.Length;
dx->Bar0Va = MmMapIoSpace(memDescTranslated->u.Memory.Start, dx->Bar0Length, MmNonCached);
if (!dx->Bar0Va) return STATUS_INSUFFICIENT_RESOURCES;

dx->CommonCfg = (volatile uint8_t*)dx->Bar0Va + caps.common_cfg.offset;
dx->NotifyBase = (volatile uint8_t*)dx->Bar0Va + caps.notify_cfg.offset;
dx->IsrStatus = (volatile uint8_t*)dx->Bar0Va + caps.isr_cfg.offset;   // 1 byte, read-to-clear
dx->DeviceCfg = (volatile uint8_t*)dx->Bar0Va + caps.device_cfg.offset;
dx->NotifyOffMultiplier = caps.notify_off_multiplier;
```

The `VIRTIOSND_TRANSPORT` helper (`virtio_pci_modern_wdm.h`) implements this mapping logic for BAR0 and computes all of the
capability VAs for you.

Use `READ_REGISTER_*` / `WRITE_REGISTER_*` for MMIO accesses, plus explicit barriers (`KeMemoryBarrier`) around multi-step sequences.

---

## 5) Serialize CommonCfg selector registers with a spin lock

`virtio_pci_common_cfg` contains *global selector registers*:

- `device_feature_select` / `device_feature`
- `driver_feature_select` / `driver_feature`
- `queue_select` + all `queue_*` fields

These selectors affect subsequent reads/writes, so any multi-step access must be serialized across threads (e.g. DPC draining RX and a power callback querying config).

WDM-style device extension fields:

```c
KSPIN_LOCK CommonCfgLock;
```

Rule of thumb: **every** sequence that writes a selector and then touches dependent fields must hold the lock. This lock must be usable at **DISPATCH_LEVEL** (DPC safe).

In this repo, `VIRTIOSND_TRANSPORT.CommonCfgLock` is initialized by `VirtIoSndTransportInit()`, and the exported
`VirtIoSndTransport*` helpers serialize selector-based `common_cfg` access internally.

---

## 6) Negotiate features (RESET → ACK → DRIVER → FEATURES_OK), require `VIRTIO_F_VERSION_1`

Modern virtio feature negotiation is 64-bit using the `*_feature_select` selector pattern.

Required status sequence:

1. `device_status = 0` (reset)
2. set `ACKNOWLEDGE`, then `DRIVER`
3. read device features (low 32, high 32)
4. compute negotiated features:
   - `negotiated = (device & wanted) | required`
   - **always** include `VIRTIO_F_VERSION_1` (bit 32) per Aero contract v1
5. write driver features (low 32, high 32)
6. set `FEATURES_OK`
7. read back `device_status` and ensure `FEATURES_OK` stuck; otherwise the device rejected features → set `FAILED`.

Minimal pseudo-code:

```c
// PASSIVE_LEVEL recommended (can sleep/stall, does multiple MMIO ops).
WriteU8(&cc->device_status, 0);
WriteU8(&cc->device_status, VIRTIO_STATUS_ACKNOWLEDGE);
WriteU8(&cc->device_status, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

UINT64 device = ReadDeviceFeatures64(cc, &dx->CommonCfgLock);

UINT64 required = (1ull << 32); // VIRTIO_F_VERSION_1
UINT64 wanted = required;
UINT64 negotiated = (device & wanted) | required;

WriteDriverFeatures64(cc, negotiated, &dx->CommonCfgLock);

WriteU8(&cc->device_status, ReadU8(&cc->device_status) | VIRTIO_STATUS_FEATURES_OK);
if ((ReadU8(&cc->device_status) & VIRTIO_STATUS_FEATURES_OK) == 0) {
    WriteU8(&cc->device_status, ReadU8(&cc->device_status) | VIRTIO_STATUS_FAILED);
    return STATUS_NOT_SUPPORTED;
}
```

In this repo, virtio-snd centralizes this flow in:

- `VirtIoSndTransportNegotiateFeatures()` (`virtio_pci_modern_wdm.h`)
  - Always requires `VIRTIO_F_VERSION_1` (bit 32).
  - Also requires any device/contract-specific bits (for virtio-snd contract v1: `VIRTIO_F_RING_INDIRECT_DESC`).

Only after queues + interrupts are configured should you set `DRIVER_OK`.

---

## 7) Program queues and notify via the notify region

For each virtqueue you use:

1. Select the queue with `queue_select = q`.
2. Read `queue_size` (max supported). Choose a size <= that.
3. Allocate DMA-visible queue memory (descriptor table + avail + used).
4. Program `queue_desc`, `queue_avail`, `queue_used` (64-bit physical addresses).
5. Set `queue_enable = 1`.

Pseudo-code (register-level):

```c
// DISPATCH_LEVEL safe if all memory is already allocated.
KeAcquireSpinLock(&dx->CommonCfgLock, &oldIrql);
WriteU16(&cc->queue_select, q);

USHORT max = ReadU16(&cc->queue_size);
if (max == 0) { /* queue not implemented */ }

WriteU64(&cc->queue_desc,  desc_pa);
WriteU64(&cc->queue_avail, avail_pa);
WriteU64(&cc->queue_used,  used_pa);

WriteU16(&cc->queue_enable, 1);
KeReleaseSpinLock(&dx->CommonCfgLock, oldIrql);
```

In this repo, this register sequence is implemented by:

- `VirtIoSndTransportSetupQueue()` (`virtio_pci_modern_wdm.h`)

### Notify (“kick”)

Modern virtio notifications use:

- per-queue `queue_notify_off` (from common cfg)
- global `notify_off_multiplier` (from notify cap)

Compute:

```text
notify_addr = notify_base + queue_notify_off * notify_off_multiplier
```

Then write the queue index (u16) to `notify_addr`.

```c
USHORT off;
KeAcquireSpinLock(&dx->CommonCfgLock, &oldIrql);
WriteU16(&cc->queue_select, q);
off = ReadU16(&cc->queue_notify_off);
KeReleaseSpinLock(&dx->CommonCfgLock, oldIrql);

volatile UINT16* notify = (volatile UINT16*)(dx->NotifyBase + (off * dx->NotifyOffMultiplier));
WRITE_REGISTER_USHORT((volatile USHORT*)notify, q);
```

In this repo, virtio-snd exposes the notify helpers as:

- `VirtIoSndTransportComputeNotifyAddr()`
- `VirtIoSndTransportNotifyQueue()`

Ordering rule: after you publish new descriptors + avail entries, issue a memory barrier (`KeMemoryBarrier`) before the notify write so the device never sees partially initialized ring contents.

---

## 8) INTx interrupts: ISR read-to-ack + DPC dispatch

For Aero contract v1, drivers must work with **INTx** (line-based, level-triggered, commonly shared).

### 8.1 The virtio ISR status register is read-to-clear

The ISR status capability (`cfg_type = 3`) is a single byte:

- bit 0 (`0x01`): queue interrupt
- bit 1 (`0x02`): config change

**Reading the byte acknowledges the interrupt** in the device. For INTx this is required to deassert the line; if you skip it you will usually get an **interrupt storm**.

### 8.2 Reference implementation (Task 424): Aero WDM INTx helper (virtio-snd)

In this repo, the WDM virtio-snd driver provides a small INTx helper that demonstrates the correct **read-to-ack + DPC**
pattern and handles teardown races:

- Header: `drivers/windows7/virtio-snd/include/virtiosnd_intx.h`
- Impl: `drivers/windows7/virtio-snd/src/virtiosnd_intx.c`

Entry points:

- `VirtIoSndIntxCaptureResources(Dx, ResourcesTranslated)` (finds a line interrupt resource; ignores message-signaled)
- `VirtIoSndIntxInitialize(Dx)` / `VirtIoSndIntxConnect(Dx)` / `VirtIoSndIntxDisconnect(Dx)`
- ISR: `VirtIoSndIntxIsr`
- DPC: `VirtIoSndIntxDpc`

Cause bits (from `virtiosnd_intx.h`):

- `VIRTIOSND_ISR_QUEUE` (`0x01`)
- `VIRTIOSND_ISR_CONFIG` (`0x02`)

Minimalized pseudocode (logic is the important part):

```c
BOOLEAN VirtIoSndIntxIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext)
{
    UNREFERENCED_PARAMETER(Interrupt);
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)ServiceContext;

    const UCHAR isr = READ_REGISTER_UCHAR(dx->Transport.IsrStatus); // ACK (read-to-clear)
    if (isr == 0) {
        return FALSE; // shared INTx: not for us
    }

    InterlockedOr(&dx->PendingIsrStatus, isr);
    KeInsertQueueDpc(&dx->InterruptDpc, NULL, NULL);
    return TRUE;
}

VOID VirtIoSndIntxDpc(_In_ PKDPC Dpc, _In_opt_ PVOID DeferredContext, _In_opt_ PVOID Arg1, _In_opt_ PVOID Arg2)
{
    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(Arg1);
    UNREFERENCED_PARAMETER(Arg2);

    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)DeferredContext;
    const LONG isr = InterlockedExchange(&dx->PendingIsrStatus, 0);

    if (isr & VIRTIOSND_ISR_QUEUE) {
        // Drain used rings, complete requests, etc.
    }
    if (isr & VIRTIOSND_ISR_CONFIG) {
        // Handle device config change.
    }
}
```

IRQL constraints:

- ISR: must not block; must touch only nonpaged memory; should not perform any selector-based `common_cfg` sequences.
- DPC: may acquire spin locks and drain queues; must still avoid blocking operations.

### 8.3 Connecting INTx

Windows 7 reports INTx as a `CmResourceTypeInterrupt` descriptor (with `CM_RESOURCE_INTERRUPT_MESSAGE` **not** set).

Requirements for a safe connect:

- `IsrStatus` MMIO must already be mapped (otherwise you cannot ACK/deassert the line interrupt).
- Connect using `IoConnectInterrupt` (or `IoConnectInterruptEx` if you have a unified INTx/MSI path).

In this repo, `VirtIoSndIntxCaptureResources` + `VirtIoSndIntxConnect` implement these checks and perform the connect.
