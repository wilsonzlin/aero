/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "virtio_pci_modern_wdm.h"

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

typedef struct _VIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT {
    KEVENT Event;
} VIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT, *PVIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT;

static NTSTATUS
VirtioPciWdmQueryInterfaceCompletionRoutine(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PVOID Context)
{
    PVIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT ctx;

    UNREFERENCED_PARAMETER(DeviceObject);
    UNREFERENCED_PARAMETER(Irp);

    ctx = (PVIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT)Context;
    KeSetEvent(&ctx->Event, IO_NO_INCREMENT, FALSE);
    return STATUS_MORE_PROCESSING_REQUIRED;
}

static NTSTATUS
VirtioPciWdmQueryInterface(_In_ PDEVICE_OBJECT LowerDeviceObject,
                           _In_ const GUID* InterfaceGuid,
                           _In_ USHORT InterfaceSize,
                           _In_ USHORT InterfaceVersion,
                           _Out_ PINTERFACE InterfaceOut)
{
    VIRTIO_PCI_WDM_QUERY_INTERFACE_CONTEXT ctx;
    PIRP irp;
    PIO_STACK_LOCATION irpSp;
    NTSTATUS status;

    if (LowerDeviceObject == NULL || InterfaceGuid == NULL || InterfaceOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    KeInitializeEvent(&ctx.Event, NotificationEvent, FALSE);

    irp = IoAllocateIrp(LowerDeviceObject->StackSize, FALSE);
    if (irp == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    irp->IoStatus.Status = STATUS_NOT_SUPPORTED;
    irp->IoStatus.Information = 0;

    irpSp = IoGetNextIrpStackLocation(irp);
    irpSp->MajorFunction = IRP_MJ_PNP;
    irpSp->MinorFunction = IRP_MN_QUERY_INTERFACE;
    irpSp->Parameters.QueryInterface.InterfaceType = (LPGUID)InterfaceGuid;
    irpSp->Parameters.QueryInterface.Size = InterfaceSize;
    irpSp->Parameters.QueryInterface.Version = InterfaceVersion;
    irpSp->Parameters.QueryInterface.Interface = InterfaceOut;
    irpSp->Parameters.QueryInterface.InterfaceSpecificData = NULL;

    IoSetCompletionRoutine(irp,
                           VirtioPciWdmQueryInterfaceCompletionRoutine,
                           &ctx,
                           /*InvokeOnSuccess=*/TRUE,
                           /*InvokeOnError=*/TRUE,
                           /*InvokeOnCancel=*/TRUE);

    status = IoCallDriver(LowerDeviceObject, irp);
    if (status == STATUS_PENDING) {
        KeWaitForSingleObject(&ctx.Event, Executive, KernelMode, FALSE, NULL);
    }

    status = irp->IoStatus.Status;
    IoFreeIrp(irp);
    return status;
}

static ULONG
VirtioPciReadConfig(_In_ PPCI_BUS_INTERFACE_STANDARD PciInterface,
                    _Out_writes_bytes_(Length) PVOID Buffer,
                    _In_ ULONG Offset,
                    _In_ ULONG Length)
{
    if (PciInterface == NULL || Buffer == NULL || Length == 0) {
        return 0;
    }

    if (PciInterface->ReadConfig != NULL) {
        return PciInterface->ReadConfig(PciInterface->Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
    }

    return 0;
}

static UINT8
VirtioPciModernWdmPciRead8(void* Context, UINT16 Offset)
{
    PVIRTIO_PCI_MODERN_WDM_DEVICE dev;
    UINT8 v;
    ULONG read;

    dev = (PVIRTIO_PCI_MODERN_WDM_DEVICE)Context;
    if (dev == NULL) {
        return 0;
    }

    v = 0;
    read = VirtioPciReadConfig(&dev->PciInterface, &v, Offset, sizeof(v));
    return (read == sizeof(v)) ? v : 0;
}

static UINT16
VirtioPciModernWdmPciRead16(void* Context, UINT16 Offset)
{
    PVIRTIO_PCI_MODERN_WDM_DEVICE dev;
    UINT16 v;
    ULONG read;

    dev = (PVIRTIO_PCI_MODERN_WDM_DEVICE)Context;
    if (dev == NULL) {
        return 0;
    }

    v = 0;
    read = VirtioPciReadConfig(&dev->PciInterface, &v, Offset, sizeof(v));
    return (read == sizeof(v)) ? v : 0;
}

static UINT32
VirtioPciModernWdmPciRead32(void* Context, UINT16 Offset)
{
    PVIRTIO_PCI_MODERN_WDM_DEVICE dev;
    UINT32 v;
    ULONG read;

    dev = (PVIRTIO_PCI_MODERN_WDM_DEVICE)Context;
    if (dev == NULL) {
        return 0;
    }

    v = 0;
    read = VirtioPciReadConfig(&dev->PciInterface, &v, Offset, sizeof(v));
    return (read == sizeof(v)) ? v : 0;
}

static NTSTATUS
VirtioPciModernWdmMapMmio(void* Context, UINT64 PhysicalAddress, UINT32 Length, volatile void** MappedVaOut)
{
    PVIRTIO_PCI_MODERN_WDM_DEVICE dev;
    ULONG i;

    dev = (PVIRTIO_PCI_MODERN_WDM_DEVICE)Context;
    if (dev == NULL || MappedVaOut == NULL || Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    *MappedVaOut = NULL;

    for (i = 0; i < VIRTIO_PCI_MAX_BARS; ++i) {
        const VIRTIO_PCI_MODERN_WDM_BAR* bar;
        UINT64 base;
        UINT64 len;
        UINT64 offset;
        PHYSICAL_ADDRESS pa;
        PVOID va;

        bar = &dev->Bars[i];
        if (!bar->Present || !bar->IsMemory || bar->Length == 0) {
            continue;
        }

        base = bar->Base;
        len = (UINT64)bar->Length;
        if (PhysicalAddress < base) {
            continue;
        }

        offset = PhysicalAddress - base;
        if (offset + (UINT64)Length > len) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        pa = bar->TranslatedStart;
        pa.QuadPart += (LONGLONG)offset;

        va = MmMapIoSpace(pa, (SIZE_T)Length, MmNonCached);
        if (va == NULL) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        *MappedVaOut = (volatile void*)va;
        return STATUS_SUCCESS;
    }

    return STATUS_NOT_FOUND;
}

static void
VirtioPciModernWdmUnmapMmio(void* Context, volatile void* MappedVa, UINT32 Length)
{
    UNREFERENCED_PARAMETER(Context);

    if (MappedVa == NULL || Length == 0) {
        return;
    }

    MmUnmapIoSpace((PVOID)MappedVa, (SIZE_T)Length);
}

static void
VirtioPciModernWdmStallUs(void* Context, UINT32 Microseconds)
{
    UNREFERENCED_PARAMETER(Context);
    KeStallExecutionProcessor(Microseconds);
}

static void
VirtioPciModernWdmMemoryBarrier(void* Context)
{
    UNREFERENCED_PARAMETER(Context);
    KeMemoryBarrier();
}

static void*
VirtioPciModernWdmSpinlockCreate(void* Context)
{
    PVIRTIO_PCI_MODERN_WDM_DEVICE dev;

    dev = (PVIRTIO_PCI_MODERN_WDM_DEVICE)Context;
    if (dev == NULL) {
        return NULL;
    }

    KeInitializeSpinLock(&dev->TransportCommonCfgLock);
    return &dev->TransportCommonCfgLock;
}

static void
VirtioPciModernWdmSpinlockDestroy(void* Context, void* Lock)
{
    UNREFERENCED_PARAMETER(Context);
    UNREFERENCED_PARAMETER(Lock);
}

static void
VirtioPciModernWdmSpinlockAcquire(void* Context, void* Lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE* StateOut)
{
    KIRQL oldIrql;

    UNREFERENCED_PARAMETER(Context);

    if (StateOut != NULL) {
        *StateOut = 0;
    }
    if (Lock == NULL || StateOut == NULL) {
        return;
    }

    oldIrql = KeAcquireSpinLockRaiseToDpc((KSPIN_LOCK*)Lock);
    *StateOut = (VIRTIO_PCI_MODERN_SPINLOCK_STATE)oldIrql;
}

static void
VirtioPciModernWdmSpinlockRelease(void* Context, void* Lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE State)
{
    UNREFERENCED_PARAMETER(Context);

    if (Lock == NULL) {
        return;
    }

    KeReleaseSpinLock((KSPIN_LOCK*)Lock, (KIRQL)State);
}

static void
VirtioPciModernWdmLog(void* Context, const char* Message)
{
#if VIRTIO_PCI_MODERN_WDM_ENABLE_DIAGNOSTICS
    UNREFERENCED_PARAMETER(Context);
    if (Message == NULL) {
        return;
    }
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "[virtio-pci-modern-wdm] %s\n", Message);
#else
    UNREFERENCED_PARAMETER(Context);
    UNREFERENCED_PARAMETER(Message);
#endif
}

static NTSTATUS
VirtioPciModernWdmReadBar0FromConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    ULONG bar0Low;
    ULONG bar0High;
    ULONG read;
    UINT64 base;
    BOOLEAN isIo;
    BOOLEAN is64;

    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    bar0Low = 0;
    read = VirtioPciReadConfig(&Dev->PciInterface, &bar0Low, 0x10, sizeof(bar0Low));
    if (read != sizeof(bar0Low)) {
        return STATUS_DEVICE_DATA_ERROR;
    }

    if (bar0Low == 0) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    isIo = (bar0Low & 0x1u) ? TRUE : FALSE;
    if (isIo) {
        return STATUS_NOT_SUPPORTED;
    }

    base = (UINT64)(bar0Low & ~0xFu);
    is64 = (((bar0Low >> 1) & 0x3u) == 0x2u) ? TRUE : FALSE;
    if (is64) {
        bar0High = 0;
        read = VirtioPciReadConfig(&Dev->PciInterface, &bar0High, 0x14, sizeof(bar0High));
        if (read != sizeof(bar0High)) {
            return STATUS_DEVICE_DATA_ERROR;
        }
        base |= ((UINT64)bar0High << 32);
        Dev->Bars[1].IsUpperHalf = TRUE;
    }

    Dev->Bars[0].Present = TRUE;
    Dev->Bars[0].IsMemory = TRUE;
    Dev->Bars[0].Is64Bit = is64;
    Dev->Bars[0].IsUpperHalf = FALSE;
    Dev->Bars[0].Base = (ULONGLONG)base;

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernWdmInit(_In_ PDEVICE_OBJECT LowerDeviceObject, _Out_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    NTSTATUS status;

    if (LowerDeviceObject == NULL || Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Dev, sizeof(*Dev));

    status = VirtioPciWdmQueryInterface(LowerDeviceObject,
                                        &GUID_PCI_BUS_INTERFACE_STANDARD,
                                        (USHORT)sizeof(Dev->PciInterface),
                                        (USHORT)PCI_BUS_INTERFACE_STANDARD_VERSION,
                                        (PINTERFACE)&Dev->PciInterface);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernWdmUninit(Dev);
        return status;
    }

    if (Dev->PciInterface.InterfaceReference != NULL) {
        Dev->PciInterface.InterfaceReference(Dev->PciInterface.Context);
        Dev->PciInterfaceAcquired = TRUE;
    }

    RtlZeroMemory(&Dev->Os, sizeof(Dev->Os));
    Dev->Os.Context = Dev;
    Dev->Os.PciRead8 = VirtioPciModernWdmPciRead8;
    Dev->Os.PciRead16 = VirtioPciModernWdmPciRead16;
    Dev->Os.PciRead32 = VirtioPciModernWdmPciRead32;
    Dev->Os.MapMmio = VirtioPciModernWdmMapMmio;
    Dev->Os.UnmapMmio = VirtioPciModernWdmUnmapMmio;
    Dev->Os.StallUs = VirtioPciModernWdmStallUs;
    Dev->Os.MemoryBarrier = VirtioPciModernWdmMemoryBarrier;
    Dev->Os.SpinlockCreate = VirtioPciModernWdmSpinlockCreate;
    Dev->Os.SpinlockDestroy = VirtioPciModernWdmSpinlockDestroy;
    Dev->Os.SpinlockAcquire = VirtioPciModernWdmSpinlockAcquire;
    Dev->Os.SpinlockRelease = VirtioPciModernWdmSpinlockRelease;
    Dev->Os.Log = VirtioPciModernWdmLog;

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernWdmMapBars(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                          _In_ PCM_RESOURCE_LIST ResourcesRaw,
                          _In_ PCM_RESOURCE_LIST ResourcesTranslated)
{
    NTSTATUS status;
    ULONG listIndex;
    ULONG barIndex;
    UINT32 barLen32;
    VIRTIO_PCI_MODERN_TRANSPORT_MODE mode;

    if (Dev == NULL || ResourcesRaw == NULL || ResourcesTranslated == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    VirtioPciModernWdmUnmapBars(Dev);

    status = VirtioPciModernWdmReadBar0FromConfig(Dev);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Locate BAR0 in the raw + translated CM resource lists. */
    barIndex = 0;
    for (listIndex = 0; listIndex < ResourcesRaw->Count && listIndex < ResourcesTranslated->Count; ++listIndex) {
        PCM_FULL_RESOURCE_DESCRIPTOR rawFull;
        PCM_FULL_RESOURCE_DESCRIPTOR transFull;
        ULONG count;
        ULONG i;

        rawFull = &ResourcesRaw->List[listIndex];
        transFull = &ResourcesTranslated->List[listIndex];

        count = rawFull->PartialResourceList.Count;
        if (transFull->PartialResourceList.Count < count) {
            count = transFull->PartialResourceList.Count;
        }

        for (i = 0; i < count; ++i) {
            PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
            PCM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;

            rawDesc = &rawFull->PartialResourceList.PartialDescriptors[i];
            transDesc = &transFull->PartialResourceList.PartialDescriptors[i];

            if (rawDesc->Type != CmResourceTypeMemory || transDesc->Type != CmResourceTypeMemory) {
                continue;
            }

            if ((UINT64)rawDesc->u.Memory.Start.QuadPart != (UINT64)Dev->Bars[barIndex].Base) {
                continue;
            }

            Dev->Bars[barIndex].RawStart = rawDesc->u.Memory.Start;
            Dev->Bars[barIndex].TranslatedStart = transDesc->u.Memory.Start;
            Dev->Bars[barIndex].Length = (SIZE_T)transDesc->u.Memory.Length;

            if (Dev->Bars[barIndex].Length == 0) {
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            goto BarFound;
        }
    }

    return STATUS_RESOURCE_TYPE_NOT_FOUND;

BarFound:
    if (Dev->Bars[0].Length > 0xFFFFFFFFu) {
        return STATUS_NOT_SUPPORTED;
    }

    barLen32 = (UINT32)Dev->Bars[0].Length;

    /* Default to strict contract enforcement unless explicitly relaxed. */
    mode = VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT;
#if !AERO_VIRTIO_PCI_ENFORCE_REVISION_ID
    mode = VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT;
#endif

    status = VirtioPciModernTransportInit(&Dev->Transport, &Dev->Os, mode, (UINT64)Dev->Bars[0].Base, barLen32);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Expose convenience fields for callers that directly consume pointers. */
    Dev->PciRevisionId = Dev->Transport.PciRevisionId;

    Dev->CommonCfg = Dev->Transport.CommonCfg;
    Dev->NotifyBase = Dev->Transport.NotifyBase;
    Dev->NotifyOffMultiplier = Dev->Transport.NotifyOffMultiplier;
    Dev->NotifyLength = (SIZE_T)Dev->Transport.NotifyLength;
    Dev->IsrStatus = Dev->Transport.IsrStatus;
    Dev->DeviceCfg = Dev->Transport.DeviceCfg;

    Dev->Bars[0].Va = (PVOID)Dev->Transport.Bar0Va;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmUnmapBars(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    ULONG i;

    if (Dev == NULL) {
        return;
    }

    VirtioPciModernTransportUninit(&Dev->Transport);

    Dev->PciRevisionId = 0;
    Dev->CommonCfg = NULL;
    Dev->NotifyBase = NULL;
    Dev->NotifyOffMultiplier = 0;
    Dev->NotifyLength = 0;
    Dev->IsrStatus = NULL;
    Dev->DeviceCfg = NULL;

    if (Dev->QueueNotifyAddrCache != NULL && Dev->QueueNotifyAddrCacheCount != 0) {
        RtlZeroMemory((PVOID)Dev->QueueNotifyAddrCache, (SIZE_T)Dev->QueueNotifyAddrCacheCount * sizeof(Dev->QueueNotifyAddrCache[0]));
    }

    for (i = 0; i < VIRTIO_PCI_MAX_BARS; ++i) {
        Dev->Bars[i].RawStart.QuadPart = 0;
        Dev->Bars[i].TranslatedStart.QuadPart = 0;
        Dev->Bars[i].Length = 0;
        Dev->Bars[i].Va = NULL;
    }
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmUninit(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL) {
        return;
    }

    VirtioPciModernWdmUnmapBars(Dev);

    if (Dev->PciInterfaceAcquired && Dev->PciInterface.InterfaceDereference != NULL) {
        Dev->PciInterface.InterfaceDereference(Dev->PciInterface.Context);
        Dev->PciInterfaceAcquired = FALSE;
    }

    RtlZeroMemory(Dev, sizeof(*Dev));
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmDumpCaps(_In_ const VIRTIO_PCI_MODERN_WDM_DEVICE* Dev)
{
#if VIRTIO_PCI_MODERN_WDM_ENABLE_DIAGNOSTICS
    if (Dev == NULL) {
        return;
    }

    DbgPrintEx(DPFLTR_IHVDRIVER_ID,
               DPFLTR_INFO_LEVEL,
               "[virtio-pci-modern-wdm] init: err=%s cap=%s\n",
               VirtioPciModernTransportInitErrorStr(Dev->Transport.InitError),
               VirtioPciModernTransportCapParseResultStr(Dev->Transport.CapParseResult));
#else
    UNREFERENCED_PARAMETER(Dev);
#endif
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmDumpBars(_In_ const VIRTIO_PCI_MODERN_WDM_DEVICE* Dev)
{
#if VIRTIO_PCI_MODERN_WDM_ENABLE_DIAGNOSTICS
    ULONG i;

    if (Dev == NULL) {
        return;
    }

    for (i = 0; i < VIRTIO_PCI_MAX_BARS; ++i) {
        const VIRTIO_PCI_MODERN_WDM_BAR* bar = &Dev->Bars[i];
        if (!bar->Present) {
            continue;
        }

        DbgPrintEx(DPFLTR_IHVDRIVER_ID,
                   DPFLTR_INFO_LEVEL,
                   "[virtio-pci-modern-wdm] BAR%lu: base=%I64x raw=%I64x trans=%I64x len=%Iu va=%p\n",
                   i,
                   (ULONGLONG)bar->Base,
                   (ULONGLONG)bar->RawStart.QuadPart,
                   (ULONGLONG)bar->TranslatedStart.QuadPart,
                   bar->Length,
                   bar->Va);
    }
#else
    UNREFERENCED_PARAMETER(Dev);
#endif
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgAcquire(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _Out_ PKIRQL OldIrql)
{
    if (OldIrql == NULL) {
        return;
    }
    *OldIrql = PASSIVE_LEVEL;

    if (Dev == NULL) {
        return;
    }

    *OldIrql = KeAcquireSpinLockRaiseToDpc(&Dev->TransportCommonCfgLock);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgRelease(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ KIRQL OldIrql)
{
    if (Dev == NULL) {
        return;
    }

    KeReleaseSpinLock(&Dev->TransportCommonCfgLock, OldIrql);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciResetDevice(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL) {
        return;
    }

    VirtioPciModernTransportResetDevice(&Dev->Transport);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciAddStatus(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UCHAR Bits)
{
    if (Dev == NULL) {
        return;
    }

    VirtioPciModernTransportAddStatus(&Dev->Transport, Bits);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UCHAR
VirtioPciGetStatus(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL) {
        return 0;
    }

    return VirtioPciModernTransportGetStatus(&Dev->Transport);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciFailDevice(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FAILED);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64
VirtioPciReadDeviceFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL) {
        return 0;
    }

    return VirtioPciModernTransportReadDeviceFeatures(&Dev->Transport);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteDriverFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UINT64 Features)
{
    if (Dev == NULL) {
        return;
    }

    VirtioPciModernTransportWriteDriverFeatures(&Dev->Transport, Features);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UINT64 Required, _In_ UINT64 Wanted, _Out_ UINT64* NegotiatedOut)
{
    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioPciModernTransportNegotiateFeatures(&Dev->Transport, Required, Wanted, NegotiatedOut);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciReadDeviceConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                          _In_ ULONG Offset,
                          _Out_writes_bytes_(Length) PVOID Buffer,
                          _In_ ULONG Length)
{
    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioPciModernTransportReadDeviceConfig(&Dev->Transport, (UINT32)Offset, Buffer, (UINT32)Length);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciWriteDeviceConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                           _In_ ULONG Offset,
                           _In_reads_bytes_(Length) const VOID* Buffer,
                           _In_ ULONG Length)
{
    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioPciModernTransportWriteDeviceConfig(&Dev->Transport, (UINT32)Offset, Buffer, (UINT32)Length);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciGetNumQueues(_In_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev)
{
    if (Dev == NULL) {
        return 0;
    }

    return VirtioPciModernTransportGetNumQueues(&Dev->Transport);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueSize(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex, _Out_ USHORT* SizeOut)
{
    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioPciModernTransportGetQueueSize(&Dev->Transport, QueueIndex, SizeOut);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciSetupQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                    _In_ USHORT QueueIndex,
                    _In_ ULONGLONG DescPa,
                    _In_ ULONGLONG AvailPa,
                    _In_ ULONGLONG UsedPa)
{
    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioPciModernTransportSetupQueue(&Dev->Transport, QueueIndex, DescPa, AvailPa, UsedPa);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciDisableQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex)
{
    if (Dev == NULL) {
        return;
    }

    VirtioPciModernTransportDisableQueue(&Dev->Transport, QueueIndex);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueNotifyAddress(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                               _In_ USHORT QueueIndex,
                               _Out_ volatile UINT16** NotifyAddrOut)
{
    UINT64 byteOff;

    if (NotifyAddrOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *NotifyAddrOut = NULL;

    if (Dev == NULL || Dev->CommonCfg == NULL || Dev->NotifyBase == NULL || Dev->NotifyOffMultiplier == 0 || Dev->NotifyLength < sizeof(UINT16)) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Dev->Transport.Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT) {
        /* Contract v1: queue_notify_off(q) == q. */
        if (QueueIndex >= Dev->CommonCfg->num_queues) {
            return STATUS_NOT_FOUND;
        }

        byteOff = (UINT64)QueueIndex * (UINT64)Dev->NotifyOffMultiplier;
        if (byteOff + sizeof(UINT16) > (UINT64)Dev->NotifyLength) {
            return STATUS_INVALID_PARAMETER;
        }

        *NotifyAddrOut = (volatile UINT16*)((volatile UCHAR*)Dev->NotifyBase + (SIZE_T)byteOff);
        return STATUS_SUCCESS;
    }

    /*
     * COMPAT: queue_notify_off may differ from queue index; read it once under
     * the canonical transport selector lock.
     */
    {
        VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
        UINT16 qsz;
        UINT16 notifyOff;

        state = 0;
        Dev->Os.SpinlockAcquire(Dev->Os.Context, Dev->Transport.CommonCfgLock, &state);

        Dev->CommonCfg->queue_select = QueueIndex;
        KeMemoryBarrier();
        qsz = Dev->CommonCfg->queue_size;
        notifyOff = Dev->CommonCfg->queue_notify_off;
        KeMemoryBarrier();

        Dev->Os.SpinlockRelease(Dev->Os.Context, Dev->Transport.CommonCfgLock, state);

        if (qsz == 0) {
            return STATUS_NOT_FOUND;
        }

        byteOff = (UINT64)notifyOff * (UINT64)Dev->NotifyOffMultiplier;
        if (byteOff + sizeof(UINT16) > (UINT64)Dev->NotifyLength) {
            return STATUS_INVALID_PARAMETER;
        }

        *NotifyAddrOut = (volatile UINT16*)((volatile UCHAR*)Dev->NotifyBase + (SIZE_T)byteOff);
        return STATUS_SUCCESS;
    }
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciNotifyQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex)
{
    volatile UINT16* notifyAddr;

    if (Dev == NULL) {
        return;
    }

    notifyAddr = NULL;
    if (Dev->QueueNotifyAddrCache != NULL && QueueIndex < Dev->QueueNotifyAddrCacheCount) {
        notifyAddr = Dev->QueueNotifyAddrCache[QueueIndex];
    }

    if (notifyAddr == NULL) {
        if (!NT_SUCCESS(VirtioPciGetQueueNotifyAddress(Dev, QueueIndex, &notifyAddr))) {
            return;
        }

        if (Dev->QueueNotifyAddrCache != NULL && QueueIndex < Dev->QueueNotifyAddrCacheCount) {
            Dev->QueueNotifyAddrCache[QueueIndex] = notifyAddr;
        }
    }

    WRITE_REGISTER_USHORT((volatile USHORT*)notifyAddr, QueueIndex);
    KeMemoryBarrier();
}

