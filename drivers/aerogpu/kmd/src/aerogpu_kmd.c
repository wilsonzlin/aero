#include "aerogpu_kmd.h"

/*
 * WDDM miniport entrypoint from dxgkrnl.
 *
 * The WDK import library provides the symbol, but it is declared here to avoid
 * relying on non-universal headers.
 */
NTSTATUS APIENTRY DxgkInitialize(_In_ PDRIVER_OBJECT DriverObject,
                                 _In_ PUNICODE_STRING RegistryPath,
                                 _Inout_ PDXGK_INITIALIZATION_DATA InitializationData);

/* ---- EDID (single virtual monitor) ------------------------------------- */

static const UCHAR g_AeroGpuEdid[128] = {
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x04, 0xB2, 0x01, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x23, 0x01, 0x03, 0x80, 0x34, 0x1D, 0x78,
    0x0A, 0xA5, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54, 0xA5, 0x4B, 0x00, 0x21,
    0x08, 0x00, 0x45, 0x40, 0x61, 0x40, 0x81, 0xC0, 0x8C, 0xC0, 0xD1, 0xC0,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x3A, 0x80, 0x18, 0x71, 0x38,
    0x2D, 0x40, 0x58, 0x2C, 0x45, 0x00, 0x08, 0x22, 0x21, 0x00, 0x00, 0x1E,
    0x00, 0x00, 0x00, 0xFC, 0x00, 0x41, 0x65, 0x72, 0x6F, 0x47, 0x50, 0x55,
    0x20, 0x4D, 0x6F, 0x6E, 0x69, 0x74, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x30,
    0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x0A,
    0x00, 0x00, 0x00, 0xFD, 0x00, 0x38, 0x4C, 0x1E, 0x53, 0x11, 0x00, 0x0A,
    0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x00, 0x45, 0x00
};

/* ---- DMA buffer private data plumbing ---------------------------------- */

typedef struct _AEROGPU_DMA_PRIV {
    ULONG Type;              /* aerogpu_submission_type */
    ULONG Reserved0;
    AEROGPU_SUBMISSION_META* Meta; /* optional */
} AEROGPU_DMA_PRIV;

/* ---- Helpers ------------------------------------------------------------ */

static VOID AeroGpuLogSubmission(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONG Fence, _In_ ULONG Type, _In_ ULONG DmaSize)
{
    ULONG idx = Adapter->SubmissionLog.WriteIndex++ % AEROGPU_SUBMISSION_LOG_SIZE;
    Adapter->SubmissionLog.Entries[idx].Fence = Fence;
    Adapter->SubmissionLog.Entries[idx].Type = Type;
    Adapter->SubmissionLog.Entries[idx].DmaSize = DmaSize;
    Adapter->SubmissionLog.Entries[idx].Qpc = KeQueryPerformanceCounter(NULL);
}

static PVOID AeroGpuAllocContiguous(_In_ SIZE_T Size, _Out_ PHYSICAL_ADDRESS* Pa)
{
    PHYSICAL_ADDRESS low;
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS boundary;

    low.QuadPart = 0;
    boundary.QuadPart = 0;
    high.QuadPart = ~0ULL;

    PVOID va = MmAllocateContiguousMemorySpecifyCache(Size, low, high, boundary, MmNonCached);
    if (!va) {
        return NULL;
    }

    RtlZeroMemory(va, Size);
    *Pa = MmGetPhysicalAddress(va);
    return va;
}

static VOID AeroGpuFreeContiguous(_In_opt_ PVOID Va)
{
    if (Va) {
        MmFreeContiguousMemory(Va);
    }
}

static VOID AeroGpuProgramScanout(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ PHYSICAL_ADDRESS FbPa)
{
    const ULONG enable = Adapter->SourceVisible ? 1u : 0u;

    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_SCANOUT_FB_LO, FbPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_SCANOUT_FB_HI, (ULONG)(FbPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_SCANOUT_PITCH, Adapter->CurrentPitch);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_SCANOUT_WIDTH, Adapter->CurrentWidth);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_SCANOUT_HEIGHT, Adapter->CurrentHeight);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_SCANOUT_FORMAT, Adapter->CurrentFormat);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_SCANOUT_ENABLE, enable);
}

static NTSTATUS AeroGpuRingInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    Adapter->RingEntryCount = AEROGPU_RING_ENTRY_COUNT_DEFAULT;
    Adapter->RingTail = 0;

    const SIZE_T ringBytes = Adapter->RingEntryCount * sizeof(aerogpu_ring_entry);
    Adapter->RingVa = AeroGpuAllocContiguous(ringBytes, &Adapter->RingPa);
    if (!Adapter->RingVa) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_RING_BASE_LO, Adapter->RingPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_RING_BASE_HI, (ULONG)(Adapter->RingPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_RING_ENTRY_COUNT, Adapter->RingEntryCount);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_RING_HEAD, 0);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_RING_TAIL, 0);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_INT_ACK, 0xFFFFFFFFu);

    return STATUS_SUCCESS;
}

static VOID AeroGpuRingCleanup(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    AeroGpuFreeContiguous(Adapter->RingVa);
    Adapter->RingVa = NULL;
    Adapter->RingPa.QuadPart = 0;
    Adapter->RingEntryCount = 0;
    Adapter->RingTail = 0;
}

static NTSTATUS AeroGpuRingPushSubmit(_Inout_ AEROGPU_ADAPTER* Adapter,
                                     _In_ ULONG Fence,
                                     _In_ ULONG DescSize,
                                     _In_ PHYSICAL_ADDRESS DescPa)
{
    if (!Adapter->RingVa || !Adapter->Bar0) {
        return STATUS_DEVICE_NOT_READY;
    }

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->RingLock, &oldIrql);

    ULONG head = AeroGpuReadRegU32(Adapter, AEROGPU_REG_RING_HEAD);
    ULONG nextTail = (Adapter->RingTail + 1) % Adapter->RingEntryCount;
    if (nextTail == head) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
    }

    aerogpu_ring_entry* ring = (aerogpu_ring_entry*)Adapter->RingVa;
    ring[Adapter->RingTail].submit.type = AEROGPU_RING_ENTRY_SUBMIT;
    ring[Adapter->RingTail].submit.flags = 0;
    ring[Adapter->RingTail].submit.fence = Fence;
    ring[Adapter->RingTail].submit.desc_size = DescSize;
    ring[Adapter->RingTail].submit.desc_gpa = (aerogpu_u64)DescPa.QuadPart;

    KeMemoryBarrier();
    Adapter->RingTail = nextTail;
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_RING_TAIL, Adapter->RingTail);
    AeroGpuWriteRegU32(Adapter, AEROGPU_REG_RING_DOORBELL, 1);

    KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
    return STATUS_SUCCESS;
}

static VOID AeroGpuFreeAllPendingSubmissions(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);

    while (!IsListEmpty(&Adapter->PendingSubmissions)) {
        PLIST_ENTRY entry = RemoveHeadList(&Adapter->PendingSubmissions);
        AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);

        KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

        AeroGpuFreeContiguous(sub->DmaCopyVa);
        AeroGpuFreeContiguous(sub->DescVa);
        if (sub->Meta) {
            ExFreePoolWithTag(sub->Meta, AEROGPU_POOL_TAG);
        }
        ExFreePoolWithTag(sub, AEROGPU_POOL_TAG);

        KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);
    }

    KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
}

static VOID AeroGpuRetireSubmissionsUpToFence(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONG CompletedFence)
{
    for (;;) {
        AEROGPU_SUBMISSION* sub = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);
        if (!IsListEmpty(&Adapter->PendingSubmissions)) {
            PLIST_ENTRY entry = Adapter->PendingSubmissions.Flink;
            AEROGPU_SUBMISSION* candidate = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
            if (candidate->Fence <= CompletedFence) {
                RemoveEntryList(entry);
                sub = candidate;
            }
        }
        KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

        if (!sub) {
            break;
        }

        AeroGpuFreeContiguous(sub->DmaCopyVa);
        AeroGpuFreeContiguous(sub->DescVa);
        if (sub->Meta) {
            ExFreePoolWithTag(sub->Meta, AEROGPU_POOL_TAG);
        }
        ExFreePoolWithTag(sub, AEROGPU_POOL_TAG);
    }
}

/* ---- DxgkDdi* ----------------------------------------------------------- */

static NTSTATUS APIENTRY AeroGpuDdiAddDevice(_In_ PDEVICE_OBJECT PhysicalDeviceObject,
                                             _Outptr_ PVOID* MiniportDeviceContext)
{
    if (!MiniportDeviceContext) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_ADAPTER* adapter =
        (AEROGPU_ADAPTER*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*adapter), AEROGPU_POOL_TAG);
    if (!adapter) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(adapter, sizeof(*adapter));

    adapter->PhysicalDeviceObject = PhysicalDeviceObject;
    KeInitializeSpinLock(&adapter->RingLock);
    KeInitializeSpinLock(&adapter->PendingLock);
    InitializeListHead(&adapter->PendingSubmissions);

    adapter->CurrentWidth = 1024;
    adapter->CurrentHeight = 768;
    adapter->CurrentPitch = 1024 * 4;
    adapter->CurrentFormat = AEROGPU_SCANOUT_X8R8G8B8;
    adapter->SourceVisible = TRUE;

    *MiniportDeviceContext = adapter;
    AEROGPU_LOG0("AddDevice");
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiStartDevice(_In_ const PVOID MiniportDeviceContext,
                                               _In_ PDXGK_START_INFO DxgkStartInfo,
                                               _In_ PDXGKRNL_INTERFACE DxgkInterface,
                                               _Out_ PULONG NumberOfVideoPresentSources,
                                               _Out_ PULONG NumberOfChildren)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter || !DxgkStartInfo || !DxgkInterface || !NumberOfVideoPresentSources || !NumberOfChildren) {
        return STATUS_INVALID_PARAMETER;
    }

    adapter->StartInfo = *DxgkStartInfo;
    adapter->DxgkInterface = *DxgkInterface;

    *NumberOfVideoPresentSources = 1;
    *NumberOfChildren = 1;

    PCM_RESOURCE_LIST resList = DxgkStartInfo->TranslatedResourceList;
    if (!resList || resList->Count < 1) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    adapter->Bar0 = NULL;
    adapter->Bar0Length = 0;

    PCM_FULL_RESOURCE_DESCRIPTOR full = &resList->List[0];
    PCM_PARTIAL_RESOURCE_LIST partial = &full->PartialResourceList;
    for (ULONG i = 0; i < partial->Count; ++i) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = &partial->PartialDescriptors[i];
        if (desc->Type == CmResourceTypeMemory) {
            adapter->Bar0Length = desc->u.Memory.Length;
            adapter->Bar0 = (PUCHAR)MmMapIoSpace(desc->u.Memory.Start, adapter->Bar0Length, MmNonCached);
            break;
        }
    }

    if (!adapter->Bar0) {
        AEROGPU_LOG0("StartDevice: BAR0 not found");
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    const ULONG magic = AeroGpuReadRegU32(adapter, AEROGPU_REG_MAGIC);
    const ULONG version = AeroGpuReadRegU32(adapter, AEROGPU_REG_VERSION);
    AEROGPU_LOG("StartDevice: MMIO magic=0x%08lx version=0x%08lx", magic, version);

    if (adapter->DxgkInterface.DxgkCbRegisterInterrupt) {
        NTSTATUS st = adapter->DxgkInterface.DxgkCbRegisterInterrupt(adapter->StartInfo.hDxgkHandle);
        if (!NT_SUCCESS(st)) {
            AEROGPU_LOG("StartDevice: DxgkCbRegisterInterrupt failed 0x%08lx", st);
        }
    }

    if (adapter->DxgkInterface.DxgkCbEnableInterrupt) {
        adapter->DxgkInterface.DxgkCbEnableInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    NTSTATUS ringSt = AeroGpuRingInit(adapter);
    if (!NT_SUCCESS(ringSt)) {
        return ringSt;
    }

    /*
     * Program an initial scanout configuration. A real modeset will come
     * through CommitVidPn + SetVidPnSourceAddress later.
     */
    {
        PHYSICAL_ADDRESS zero;
        zero.QuadPart = 0;
        AeroGpuProgramScanout(adapter, zero);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiStopDevice(_In_ const PVOID MiniportDeviceContext)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_LOG0("StopDevice");

    if (adapter->DxgkInterface.DxgkCbDisableInterrupt) {
        adapter->DxgkInterface.DxgkCbDisableInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    if (adapter->DxgkInterface.DxgkCbUnregisterInterrupt) {
        adapter->DxgkInterface.DxgkCbUnregisterInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    AeroGpuFreeAllPendingSubmissions(adapter);
    AeroGpuRingCleanup(adapter);

    if (adapter->Bar0) {
        MmUnmapIoSpace(adapter->Bar0, adapter->Bar0Length);
        adapter->Bar0 = NULL;
        adapter->Bar0Length = 0;
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRemoveDevice(_In_ const PVOID MiniportDeviceContext)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_LOG0("RemoveDevice");
    ExFreePoolWithTag(adapter, AEROGPU_POOL_TAG);
    return STATUS_SUCCESS;
}

static VOID APIENTRY AeroGpuDdiUnload(VOID)
{
    AEROGPU_LOG0("Unload");
}

static NTSTATUS APIENTRY AeroGpuDdiQueryAdapterInfo(_In_ const HANDLE hAdapter,
                                                    _In_ const DXGKARG_QUERYADAPTERINFO* pQueryAdapterInfo)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pQueryAdapterInfo || !pQueryAdapterInfo->pOutputData) {
        return STATUS_INVALID_PARAMETER;
    }

    switch (pQueryAdapterInfo->Type) {
    case DXGKQAITYPE_DRIVERCAPS: {
        if (pQueryAdapterInfo->OutputDataSize < sizeof(DXGK_DRIVERCAPS)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        DXGK_DRIVERCAPS* caps = (DXGK_DRIVERCAPS*)pQueryAdapterInfo->pOutputData;
        RtlZeroMemory(caps, sizeof(*caps));
        caps->WDDMVersion = DXGKDDI_WDDMv1_1;
        caps->HighestAcceptableAddress.QuadPart = ~0ULL;
        caps->MaxAllocationListSlotId = 0xFFFF;
        caps->MaxPatchLocationListSlotId = 0xFFFF;
        caps->DmaBufferPrivateDataSize = sizeof(AEROGPU_DMA_PRIV);
        caps->SchedulingCaps.Value = 0;
        caps->SchedulingCaps.MultipleEngineAware = 0;
        caps->PreemptionCaps.GraphicsPreemptionGranularity = D3DKMDT_GRAPHICS_PREEMPTION_DMA_BUFFER_BOUNDARY;
        caps->PreemptionCaps.ComputePreemptionGranularity = D3DKMDT_COMPUTE_PREEMPTION_DMA_BUFFER_BOUNDARY;
        return STATUS_SUCCESS;
    }

    case DXGKQAITYPE_QUERYSEGMENT: {
        if (pQueryAdapterInfo->OutputDataSize < sizeof(DXGK_QUERYSEGMENTOUT)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        DXGK_QUERYSEGMENTOUT* out = (DXGK_QUERYSEGMENTOUT*)pQueryAdapterInfo->pOutputData;
        RtlZeroMemory(out, sizeof(*out));

        out->NbSegments = 1;
        out->pSegmentDescriptor[0].BaseAddress.QuadPart = 0;
        out->pSegmentDescriptor[0].Size = 512ull * 1024ull * 1024ull;
        out->pSegmentDescriptor[0].Flags.Value = 0;
        out->pSegmentDescriptor[0].Flags.Aperture = 1;
        out->pSegmentDescriptor[0].Flags.CpuVisible = 1;
        out->pSegmentDescriptor[0].Flags.CacheCoherent = 1;
        out->pSegmentDescriptor[0].MemorySegmentGroup = DXGK_MEMORY_SEGMENT_GROUP_NON_LOCAL;

        out->PagingBufferPrivateDataSize = sizeof(AEROGPU_DMA_PRIV);
        out->PagingBufferSegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        out->PagingBufferSize = 0;
        return STATUS_SUCCESS;
    }

    case DXGKQAITYPE_GETSEGMENTGROUPSIZE: {
        if (pQueryAdapterInfo->OutputDataSize < sizeof(DXGK_SEGMENTGROUPSIZE)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        DXGK_SEGMENTGROUPSIZE* sizes = (DXGK_SEGMENTGROUPSIZE*)pQueryAdapterInfo->pOutputData;
        RtlZeroMemory(sizes, sizeof(*sizes));
        sizes->LocalMemorySize = 0;
        sizes->NonLocalMemorySize = 512ull * 1024ull * 1024ull;
        return STATUS_SUCCESS;
    }

    case DXGKQAITYPE_UMDRIVERPRIVATE: {
        /* Provide a minimal version blob for bring-up. */
        if (pQueryAdapterInfo->OutputDataSize < sizeof(ULONG)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        *(ULONG*)pQueryAdapterInfo->pOutputData = AEROGPU_MMIO_VERSION;
        return STATUS_SUCCESS;
    }

    default:
        return STATUS_NOT_SUPPORTED;
    }
}

static NTSTATUS APIENTRY AeroGpuDdiQueryChildRelations(_In_ const HANDLE hAdapter,
                                                      _Inout_ DXGKARG_QUERYCHILDRELATIONS* pRelations)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pRelations || !pRelations->pChildRelations) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pRelations->ChildRelationsCount < 1) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlZeroMemory(&pRelations->pChildRelations[0], sizeof(pRelations->pChildRelations[0]));
    pRelations->pChildRelations[0].ChildDeviceType = DXGK_CHILD_DEVICE_TYPE_MONITOR;
    pRelations->pChildRelations[0].ChildUid = AEROGPU_CHILD_UID;
    pRelations->pChildRelations[0].AcpiUid = 0;

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiQueryChildStatus(_In_ const HANDLE hAdapter,
                                                   _Inout_ DXGKARG_QUERYCHILDSTATUS* pChildStatus)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pChildStatus) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pChildStatus->ChildUid != AEROGPU_CHILD_UID) {
        return STATUS_INVALID_PARAMETER;
    }

    switch (pChildStatus->Type) {
    case StatusConnection:
        pChildStatus->HotPlug.Connected = TRUE;
        return STATUS_SUCCESS;
    default:
        return STATUS_SUCCESS;
    }
}

static NTSTATUS APIENTRY AeroGpuDdiQueryDeviceDescriptor(_In_ const HANDLE hAdapter,
                                                        _Inout_ DXGKARG_QUERYDEVICE_DESCRIPTOR* pDescriptor)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pDescriptor || !pDescriptor->pDescriptorBuffer) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pDescriptor->ChildUid != AEROGPU_CHILD_UID) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pDescriptor->DescriptorOffset >= sizeof(g_AeroGpuEdid)) {
        return STATUS_INVALID_PARAMETER;
    }

    ULONG remaining = (ULONG)sizeof(g_AeroGpuEdid) - pDescriptor->DescriptorOffset;
    ULONG toCopy = pDescriptor->DescriptorLength;
    if (toCopy > remaining) {
        toCopy = remaining;
    }
    RtlCopyMemory(pDescriptor->pDescriptorBuffer, g_AeroGpuEdid + pDescriptor->DescriptorOffset, toCopy);
    pDescriptor->DescriptorLength = toCopy;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRecommendFunctionalVidPn(_In_ const HANDLE hAdapter,
                                                           _Inout_ DXGKARG_RECOMMENDFUNCTIONALVIDPN* pRecommend)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pRecommend);
    /*
     * For bring-up we rely on EDID + dxgkrnl's VidPN construction. This driver
     * supports a single source/target and accepts whatever functional VidPN the
     * OS chooses.
     */
    return STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN;
}

static NTSTATUS APIENTRY AeroGpuDdiEnumVidPnCofuncModality(_In_ const HANDLE hAdapter,
                                                          _Inout_ DXGKARG_ENUMVIDPNCOFUNCMODALITY* pEnum)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pEnum);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiCommitVidPn(_In_ const HANDLE hAdapter, _In_ const DXGKARG_COMMITVIDPN* pCommitVidPn)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pCommitVidPn) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * A minimal implementation keeps a cached mode for scanout programming.
     * Parsing the full VidPN object is possible but intentionally deferred; the
     * Windows display stack will still provide correct pitch/address via
     * SetVidPnSourceAddress.
     */
    UNREFERENCED_PARAMETER(pCommitVidPn);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetVidPnSourceAddress(_In_ const HANDLE hAdapter,
                                                        _Inout_ const DXGKARG_SETVIDPNSOURCEADDRESS* pSetAddress)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pSetAddress) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pSetAddress->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    adapter->CurrentPitch = pSetAddress->PrimaryPitch;

    PHYSICAL_ADDRESS fb;
    fb.QuadPart = pSetAddress->PrimaryAddress.QuadPart;
    AeroGpuProgramScanout(adapter, fb);

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetVidPnSourceVisibility(_In_ const HANDLE hAdapter,
                                                           _In_ const DXGKARG_SETVIDPNSOURCEVISIBILITY* pVisibility)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pVisibility) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pVisibility->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    adapter->SourceVisible = pVisibility->Visible ? TRUE : FALSE;
    AeroGpuWriteRegU32(adapter, AEROGPU_REG_SCANOUT_ENABLE, adapter->SourceVisible ? 1u : 0u);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiUpdateActiveVidPnPresentPath(_In_ const HANDLE hAdapter,
                                                                _Inout_ DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH* pUpdate)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pUpdate);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRecommendMonitorModes(_In_ const HANDLE hAdapter,
                                                         _Inout_ DXGKARG_RECOMMENDMONITORMODES* pRecommend)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pRecommend);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiGetStandardAllocationDriverData(_In_ const HANDLE hAdapter,
                                                                   _Inout_ DXGKARG_GETSTANDARDALLOCATIONDRIVERDATA* pData)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pData || !pData->pAllocationInfo) {
        return STATUS_INVALID_PARAMETER;
    }

    DXGK_ALLOCATIONINFO* info = pData->pAllocationInfo;
    RtlZeroMemory(info, sizeof(*info));

    switch (pData->StandardAllocationType) {
    case StandardAllocationTypePrimary: {
        info->Size = (SIZE_T)adapter->CurrentPitch * (SIZE_T)adapter->CurrentHeight;
        info->Alignment = 0;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.Value = 0;
        info->Flags.Primary = 1;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        return STATUS_SUCCESS;
    }
    default:
        return STATUS_NOT_SUPPORTED;
    }
}

static NTSTATUS APIENTRY AeroGpuDdiCreateAllocation(_In_ const HANDLE hAdapter,
                                                   _Inout_ DXGKARG_CREATEALLOCATION* pCreate)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pCreate || !pCreate->pAllocationInfo) {
        return STATUS_INVALID_PARAMETER;
    }

    for (UINT i = 0; i < pCreate->NumAllocations; ++i) {
        DXGK_ALLOCATIONINFO* info = &pCreate->pAllocationInfo[i];

        AEROGPU_ALLOCATION* alloc =
            (AEROGPU_ALLOCATION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*alloc), AEROGPU_POOL_TAG);
        if (!alloc) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        alloc->AllocationId = ++adapter->NextAllocationId;
        alloc->SizeBytes = info->Size;
        alloc->Flags = 0;
        alloc->LastKnownPa.QuadPart = 0;

        info->hAllocation = (HANDLE)alloc;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        info->SupportedReadSegmentSet = 1;
        info->SupportedWriteSegmentSet = 1;
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDestroyAllocation(_In_ const HANDLE hAdapter,
                                                    _In_ const DXGKARG_DESTROYALLOCATION* pDestroy)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pDestroy) {
        return STATUS_INVALID_PARAMETER;
    }

    for (UINT i = 0; i < pDestroy->NumAllocations; ++i) {
        HANDLE hAllocation = pDestroy->pAllocationList[i].hAllocation;
        if (hAllocation) {
            ExFreePoolWithTag((PVOID)hAllocation, AEROGPU_POOL_TAG);
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDescribeAllocation(_In_ const HANDLE hAdapter,
                                                     _Inout_ DXGKARG_DESCRIBEALLOCATION* pDescribe)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pDescribe || !pDescribe->pAllocationInfo) {
        return STATUS_INVALID_PARAMETER;
    }

    DXGK_ALLOCATIONINFO* info = pDescribe->pAllocationInfo;
    AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)pDescribe->hAllocation;

    RtlZeroMemory(info, sizeof(*info));
    info->Size = alloc ? alloc->SizeBytes : 0;
    info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
    info->Flags.CpuVisible = 1;
    info->Flags.Aperture = 1;
    info->SupportedReadSegmentSet = 1;
    info->SupportedWriteSegmentSet = 1;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiOpenAllocation(_In_ const HANDLE hAdapter,
                                                 _Inout_ DXGKARG_OPENALLOCATION* pOpen)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pOpen);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiCloseAllocation(_In_ const HANDLE hAdapter,
                                                  _In_ const DXGKARG_CLOSEALLOCATION* pClose)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pClose);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiCreateDevice(_In_ const HANDLE hAdapter,
                                               _Inout_ DXGKARG_CREATEDEVICE* pCreate)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pCreate) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_DEVICE* dev =
        (AEROGPU_DEVICE*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*dev), AEROGPU_POOL_TAG);
    if (!dev) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(dev, sizeof(*dev));
    dev->Adapter = adapter;

    pCreate->hDevice = (HANDLE)dev;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDestroyDevice(_In_ const HANDLE hDevice)
{
    if (hDevice) {
        ExFreePoolWithTag((PVOID)hDevice, AEROGPU_POOL_TAG);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiCreateContext(_In_ const HANDLE hDevice,
                                                _Inout_ DXGKARG_CREATECONTEXT* pCreate)
{
    AEROGPU_DEVICE* dev = (AEROGPU_DEVICE*)hDevice;
    if (!dev || !pCreate) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_CONTEXT* ctx =
        (AEROGPU_CONTEXT*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*ctx), AEROGPU_POOL_TAG);
    if (!ctx) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(ctx, sizeof(*ctx));
    ctx->Device = dev;
    pCreate->hContext = (HANDLE)ctx;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDestroyContext(_In_ const HANDLE hContext)
{
    if (hContext) {
        ExFreePoolWithTag((PVOID)hContext, AEROGPU_POOL_TAG);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuBuildAndAttachMeta(_In_ ULONG Type,
                                                  _In_ UINT AllocationCount,
                                                  _In_reads_opt_(AllocationCount) const DXGK_ALLOCATIONLIST* AllocationList,
                                                  _Out_ AEROGPU_SUBMISSION_META** MetaOut)
{
    *MetaOut = NULL;

    SIZE_T metaSize = FIELD_OFFSET(AEROGPU_SUBMISSION_META, Allocations) +
                      ((SIZE_T)AllocationCount * sizeof(aerogpu_submission_desc_allocation));

    AEROGPU_SUBMISSION_META* meta =
        (AEROGPU_SUBMISSION_META*)ExAllocatePoolWithTag(NonPagedPool, metaSize, AEROGPU_POOL_TAG);
    if (!meta) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(meta, metaSize);

    meta->Type = Type;
    meta->AllocationCount = AllocationCount;

    for (UINT i = 0; i < AllocationCount; ++i) {
        AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)AllocationList[i].hAllocation;
        meta->Allocations[i].allocation_handle = (aerogpu_u64)(ULONG_PTR)AllocationList[i].hAllocation;
        meta->Allocations[i].gpa = (aerogpu_u64)AllocationList[i].PhysicalAddress.QuadPart;
        meta->Allocations[i].size_bytes = (aerogpu_u32)(alloc ? alloc->SizeBytes : 0);
        meta->Allocations[i].reserved0 = 0;

        if (alloc) {
            alloc->LastKnownPa.QuadPart = AllocationList[i].PhysicalAddress.QuadPart;
        }
    }

    *MetaOut = meta;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRender(_In_ const HANDLE hContext, _Inout_ DXGKARG_RENDER* pRender)
{
    UNREFERENCED_PARAMETER(hContext);
    if (!pRender || !pRender->pDmaBufferPrivateData) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pRender->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_RENDER;
    priv->Reserved0 = 0;
    priv->Meta = NULL;

    if (pRender->AllocationListSize && pRender->pAllocationList) {
        NTSTATUS st = AeroGpuBuildAndAttachMeta(AEROGPU_SUBMIT_RENDER,
                                               pRender->AllocationListSize,
                                               pRender->pAllocationList,
                                               &priv->Meta);
        if (!NT_SUCCESS(st)) {
            return st;
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiPresent(_In_ const HANDLE hContext, _Inout_ DXGKARG_PRESENT* pPresent)
{
    UNREFERENCED_PARAMETER(hContext);
    if (!pPresent || !pPresent->pDmaBufferPrivateData) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pPresent->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_PRESENT;
    priv->Reserved0 = 0;
    priv->Meta = NULL;

    if (pPresent->AllocationListSize && pPresent->pAllocationList) {
        NTSTATUS st = AeroGpuBuildAndAttachMeta(AEROGPU_SUBMIT_PRESENT,
                                               pPresent->AllocationListSize,
                                               pPresent->pAllocationList,
                                               &priv->Meta);
        if (!NT_SUCCESS(st)) {
            return st;
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiBuildPagingBuffer(_In_ const HANDLE hAdapter,
                                                    _Inout_ DXGKARG_BUILDPAGINGBUFFER* pBuildPagingBuffer)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pBuildPagingBuffer || !pBuildPagingBuffer->pDmaBufferPrivateData) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Emit no-op paging buffers; system-memory-only segment keeps paging simple. */
    pBuildPagingBuffer->DmaBufferSize = 0;
    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pBuildPagingBuffer->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_PAGING;
    priv->Reserved0 = 0;
    priv->Meta = NULL;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSubmitCommand(_In_ const HANDLE hAdapter,
                                                _In_ const DXGKARG_SUBMITCOMMAND* pSubmitCommand)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pSubmitCommand) {
        return STATUS_INVALID_PARAMETER;
    }

    const ULONG fence = pSubmitCommand->SubmissionFenceId;

    ULONG type = AEROGPU_SUBMIT_PAGING;
    AEROGPU_SUBMISSION_META* meta = NULL;
    if (pSubmitCommand->pDmaBufferPrivateData) {
        const AEROGPU_DMA_PRIV* priv = (const AEROGPU_DMA_PRIV*)pSubmitCommand->pDmaBufferPrivateData;
        type = priv->Type;
        meta = priv->Meta;
    }

    PHYSICAL_ADDRESS dmaPa;
    PVOID dmaVa = AeroGpuAllocContiguous(pSubmitCommand->DmaBufferSize, &dmaPa);
    if (!dmaVa) {
        if (meta) {
            ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
        }
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlCopyMemory(dmaVa, pSubmitCommand->pDmaBuffer, pSubmitCommand->DmaBufferSize);

    const ULONG allocCount = meta ? meta->AllocationCount : 0;
    SIZE_T descSize = sizeof(aerogpu_submission_desc_header) + (SIZE_T)allocCount * sizeof(aerogpu_submission_desc_allocation);

    PHYSICAL_ADDRESS descPa;
    aerogpu_submission_desc_header* desc = (aerogpu_submission_desc_header*)AeroGpuAllocContiguous(descSize, &descPa);
    if (!desc) {
        AeroGpuFreeContiguous(dmaVa);
        if (meta) {
            ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
        }
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    desc->version = AEROGPU_SUBMISSION_DESC_VERSION;
    desc->type = type;
    desc->fence = fence;
    desc->reserved0 = 0;
    desc->dma_buffer_gpa = (aerogpu_u64)dmaPa.QuadPart;
    desc->dma_buffer_size = pSubmitCommand->DmaBufferSize;
    desc->allocation_count = allocCount;

    if (allocCount && meta) {
        aerogpu_submission_desc_allocation* out = (aerogpu_submission_desc_allocation*)(desc + 1);
        RtlCopyMemory(out, meta->Allocations, (SIZE_T)allocCount * sizeof(*out));
    }

    NTSTATUS ringSt = AeroGpuRingPushSubmit(adapter, fence, (ULONG)descSize, descPa);
    if (!NT_SUCCESS(ringSt)) {
        AeroGpuFreeContiguous(desc);
        AeroGpuFreeContiguous(dmaVa);
        if (meta) {
            ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
        }
        return ringSt;
    }

    AEROGPU_SUBMISSION* sub =
        (AEROGPU_SUBMISSION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*sub), AEROGPU_POOL_TAG);
    if (!sub) {
        /* Submission already sent; keep resources around until reset/stop. */
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(sub, sizeof(*sub));
    sub->Fence = fence;
    sub->DmaCopyVa = dmaVa;
    sub->DmaCopySize = pSubmitCommand->DmaBufferSize;
    sub->DmaCopyPa = dmaPa;
    sub->DescVa = desc;
    sub->DescSize = descSize;
    sub->DescPa = descPa;
    sub->Meta = meta;

    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->PendingLock, &oldIrql);
    InsertTailList(&adapter->PendingSubmissions, &sub->ListEntry);
    adapter->LastSubmittedFence = fence;
    KeReleaseSpinLock(&adapter->PendingLock, oldIrql);

    AeroGpuLogSubmission(adapter, fence, type, pSubmitCommand->DmaBufferSize);

    return STATUS_SUCCESS;
}

static BOOLEAN APIENTRY AeroGpuDdiInterruptRoutine(_In_ const PVOID MiniportDeviceContext,
                                                   _In_ ULONG MessageNumber)
{
    UNREFERENCED_PARAMETER(MessageNumber);
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter || !adapter->Bar0) {
        return FALSE;
    }

    const ULONG status = AeroGpuReadRegU32(adapter, AEROGPU_REG_INT_STATUS);
    if (!(status & AEROGPU_INT_FENCE)) {
        return FALSE;
    }

    const ULONG completedFence = AeroGpuReadRegU32(adapter, AEROGPU_REG_FENCE_COMPLETED);
    AeroGpuWriteRegU32(adapter, AEROGPU_REG_INT_ACK, AEROGPU_INT_FENCE);

    adapter->LastCompletedFence = completedFence;

    if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
        DXGKARGCB_NOTIFY_INTERRUPT notify;
        RtlZeroMemory(&notify, sizeof(notify));
        notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
        notify.DmaCompleted.SubmissionFenceId = completedFence;
        notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
        notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
        adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
    }

    if (adapter->DxgkInterface.DxgkCbQueueDpcForIsr) {
        adapter->DxgkInterface.DxgkCbQueueDpcForIsr(adapter->StartInfo.hDxgkHandle);
    }

    return TRUE;
}

static VOID APIENTRY AeroGpuDdiDpcRoutine(_In_ const PVOID MiniportDeviceContext)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter) {
        return;
    }

    if (adapter->DxgkInterface.DxgkCbNotifyDpc) {
        adapter->DxgkInterface.DxgkCbNotifyDpc(adapter->StartInfo.hDxgkHandle);
    }

    AeroGpuRetireSubmissionsUpToFence(adapter, adapter->LastCompletedFence);
}

static NTSTATUS APIENTRY AeroGpuDdiResetFromTimeout(_In_ const HANDLE hAdapter)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Keep recovery simple: clear the ring pointers and treat all in-flight
     * work as completed to unblock dxgkrnl. A well-behaved emulator should not
     * require this path under normal usage.
     */
    if (adapter->Bar0) {
        AeroGpuWriteRegU32(adapter, AEROGPU_REG_RING_HEAD, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_REG_RING_TAIL, 0);
        adapter->RingTail = 0;
    }

    adapter->LastCompletedFence = adapter->LastSubmittedFence;

    if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
        DXGKARGCB_NOTIFY_INTERRUPT notify;
        RtlZeroMemory(&notify, sizeof(notify));
        notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
        notify.DmaCompleted.SubmissionFenceId = adapter->LastCompletedFence;
        notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
        notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
        adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
    }

    if (adapter->DxgkInterface.DxgkCbQueueDpcForIsr) {
        adapter->DxgkInterface.DxgkCbQueueDpcForIsr(adapter->StartInfo.hDxgkHandle);
    }

    AeroGpuFreeAllPendingSubmissions(adapter);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRestartFromTimeout(_In_ const HANDLE hAdapter)
{
    UNREFERENCED_PARAMETER(hAdapter);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetPointerPosition(_In_ const HANDLE hAdapter,
                                                     _In_ const DXGKARG_SETPOINTERPOSITION* pPos)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pPos);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetPointerShape(_In_ const HANDLE hAdapter,
                                                  _In_ const DXGKARG_SETPOINTERSHAPE* pShape)
{
    UNREFERENCED_PARAMETER(hAdapter);
    UNREFERENCED_PARAMETER(pShape);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiEscape(_In_ const HANDLE hAdapter, _Inout_ DXGKARG_ESCAPE* pEscape)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pEscape || !pEscape->pPrivateDriverData || pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_header)) {
        return STATUS_INVALID_PARAMETER;
    }

    aerogpu_escape_header* hdr = (aerogpu_escape_header*)pEscape->pPrivateDriverData;
    if (hdr->version != AEROGPU_ESCAPE_VERSION) {
        return STATUS_NOT_SUPPORTED;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_DEVICE) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_device_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_query_device_out* out = (aerogpu_escape_query_device_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
        out->hdr.size = sizeof(*out);
        out->mmio_version = adapter->Bar0 ? AeroGpuReadRegU32(adapter, AEROGPU_REG_VERSION) : 0;
        out->reserved0 = 0;
        return STATUS_SUCCESS;
    }

    return STATUS_NOT_SUPPORTED;
}

/* ---- DriverEntry -------------------------------------------------------- */

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    DXGK_INITIALIZATION_DATA init;
    RtlZeroMemory(&init, sizeof(init));
    init.Version = DXGKDDI_INTERFACE_VERSION_WDDM1_1;

    init.DxgkDdiAddDevice = AeroGpuDdiAddDevice;
    init.DxgkDdiStartDevice = AeroGpuDdiStartDevice;
    init.DxgkDdiStopDevice = AeroGpuDdiStopDevice;
    init.DxgkDdiRemoveDevice = AeroGpuDdiRemoveDevice;
    init.DxgkDdiUnload = AeroGpuDdiUnload;

    init.DxgkDdiQueryAdapterInfo = AeroGpuDdiQueryAdapterInfo;

    init.DxgkDdiQueryChildRelations = AeroGpuDdiQueryChildRelations;
    init.DxgkDdiQueryChildStatus = AeroGpuDdiQueryChildStatus;
    init.DxgkDdiQueryDeviceDescriptor = AeroGpuDdiQueryDeviceDescriptor;

    init.DxgkDdiRecommendFunctionalVidPn = AeroGpuDdiRecommendFunctionalVidPn;
    init.DxgkDdiEnumVidPnCofuncModality = AeroGpuDdiEnumVidPnCofuncModality;
    init.DxgkDdiCommitVidPn = AeroGpuDdiCommitVidPn;
    init.DxgkDdiUpdateActiveVidPnPresentPath = AeroGpuDdiUpdateActiveVidPnPresentPath;
    init.DxgkDdiRecommendMonitorModes = AeroGpuDdiRecommendMonitorModes;

    init.DxgkDdiSetVidPnSourceAddress = AeroGpuDdiSetVidPnSourceAddress;
    init.DxgkDdiSetVidPnSourceVisibility = AeroGpuDdiSetVidPnSourceVisibility;

    init.DxgkDdiCreateAllocation = AeroGpuDdiCreateAllocation;
    init.DxgkDdiDestroyAllocation = AeroGpuDdiDestroyAllocation;
    init.DxgkDdiDescribeAllocation = AeroGpuDdiDescribeAllocation;
    init.DxgkDdiGetStandardAllocationDriverData = AeroGpuDdiGetStandardAllocationDriverData;
    init.DxgkDdiOpenAllocation = AeroGpuDdiOpenAllocation;
    init.DxgkDdiCloseAllocation = AeroGpuDdiCloseAllocation;

    init.DxgkDdiCreateDevice = AeroGpuDdiCreateDevice;
    init.DxgkDdiDestroyDevice = AeroGpuDdiDestroyDevice;
    init.DxgkDdiCreateContext = AeroGpuDdiCreateContext;
    init.DxgkDdiDestroyContext = AeroGpuDdiDestroyContext;
    init.DxgkDdiRender = AeroGpuDdiRender;
    init.DxgkDdiPresent = AeroGpuDdiPresent;

    init.DxgkDdiBuildPagingBuffer = AeroGpuDdiBuildPagingBuffer;
    init.DxgkDdiSubmitCommand = AeroGpuDdiSubmitCommand;

    init.DxgkDdiInterruptRoutine = AeroGpuDdiInterruptRoutine;
    init.DxgkDdiDpcRoutine = AeroGpuDdiDpcRoutine;
    init.DxgkDdiResetFromTimeout = AeroGpuDdiResetFromTimeout;
    init.DxgkDdiRestartFromTimeout = AeroGpuDdiRestartFromTimeout;

    init.DxgkDdiSetPointerPosition = AeroGpuDdiSetPointerPosition;
    init.DxgkDdiSetPointerShape = AeroGpuDdiSetPointerShape;

    init.DxgkDdiEscape = AeroGpuDdiEscape;

    return DxgkInitialize(DriverObject, RegistryPath, &init);
}
