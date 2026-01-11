#include "aerogpu_ring.h"

#include "aerogpu_kmd.h"
#include "aerogpu_dbgctl_escape.h"
#include "aerogpu_umd_private.h"
#include "aerogpu_wddm_alloc.h"

#define AEROGPU_VBLANK_PERIOD_NS_DEFAULT 16666667u

/* Internal-only bits stored in AEROGPU_ALLOCATION::Flags (not exposed to UMD). */
#define AEROGPU_KMD_ALLOC_FLAG_OPENED 0x80000000u

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

/*
 * Read a 64-bit MMIO value exposed as two 32-bit registers in LO/HI form.
 *
 * Use an HI/LO/HI pattern to avoid tearing if the device updates the value
 * concurrently.
 */
static aerogpu_u64 AeroGpuReadRegU64HiLoHi(_In_ const AEROGPU_ADAPTER* Adapter, _In_ ULONG LoOffset, _In_ ULONG HiOffset)
{
    ULONG hi = AeroGpuReadRegU32(Adapter, HiOffset);
    for (;;) {
        const ULONG lo = AeroGpuReadRegU32(Adapter, LoOffset);
        const ULONG hi2 = AeroGpuReadRegU32(Adapter, HiOffset);
        if (hi == hi2) {
            return ((aerogpu_u64)hi << 32) | (aerogpu_u64)lo;
        }
        hi = hi2;
    }
}

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

static ULONG AeroGpuLegacyScanoutFormatToNew(_In_ ULONG LegacyFormat)
{
    switch (LegacyFormat) {
    case AEROGPU_SCANOUT_X8R8G8B8:
        /* D3DFMT_X8R8G8B8 == B8G8R8X8 in little-endian memory. */
        return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    default:
        return AEROGPU_FORMAT_INVALID;
    }
}

static VOID AeroGpuFreeSubmissionMeta(_In_opt_ AEROGPU_SUBMISSION_META* Meta)
{
    if (!Meta) {
        return;
    }

    AeroGpuFreeContiguous(Meta->AllocTableVa);
    ExFreePoolWithTag(Meta, AEROGPU_POOL_TAG);
}

static NTSTATUS AeroGpuBuildAllocTable(_In_reads_opt_(Count) const DXGK_ALLOCATIONLIST* List,
                                      _In_ UINT Count,
                                      _Outptr_result_bytebuffer_(*OutSizeBytes) PVOID* OutVa,
                                      _Out_ PHYSICAL_ADDRESS* OutPa,
                                      _Out_ UINT* OutSizeBytes)
{
    if (!OutVa || !OutPa || !OutSizeBytes) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutVa = NULL;
    OutPa->QuadPart = 0;
    *OutSizeBytes = 0;

    struct aerogpu_alloc_entry* tmpEntries = NULL;
    uint32_t* seen = NULL;
    UINT entryCount = 0;

    if (Count && List) {
        const SIZE_T tmpBytes = (SIZE_T)Count * sizeof(*tmpEntries);
        tmpEntries = (struct aerogpu_alloc_entry*)ExAllocatePoolWithTag(NonPagedPool, tmpBytes, AEROGPU_POOL_TAG);
        if (!tmpEntries) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlZeroMemory(tmpEntries, tmpBytes);

        UINT cap = 16;
        const uint64_t target = (uint64_t)Count * 2ull;
        while ((uint64_t)cap < target && cap < (1u << 30)) {
            cap <<= 1;
        }

        const SIZE_T seenBytes = (SIZE_T)cap * sizeof(*seen);
        seen = (uint32_t*)ExAllocatePoolWithTag(NonPagedPool, seenBytes, AEROGPU_POOL_TAG);
        if (!seen) {
            ExFreePoolWithTag(tmpEntries, AEROGPU_POOL_TAG);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlZeroMemory(seen, seenBytes);

        const UINT mask = cap - 1;

        for (UINT i = 0; i < Count; ++i) {
            AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)List[i].hAllocation;
            if (!alloc) {
                AEROGPU_LOG("BuildAllocTable: AllocationList[%u] has null hAllocation", i);
                continue;
            }

            alloc->LastKnownPa.QuadPart = List[i].PhysicalAddress.QuadPart;

            const uint32_t allocId = (uint32_t)alloc->AllocationId;
            if (allocId == 0) {
                AEROGPU_LOG("BuildAllocTable: AllocationList[%u] has alloc_id=0", i);
                continue;
            }

            UINT slot = (allocId * 2654435761u) & mask;
            for (;;) {
                const uint32_t existing = seen[slot];
                if (existing == 0) {
                    seen[slot] = allocId;

                    tmpEntries[entryCount].alloc_id = allocId;
                    tmpEntries[entryCount].flags = 0;
                    tmpEntries[entryCount].gpa = (uint64_t)List[i].PhysicalAddress.QuadPart;
                    tmpEntries[entryCount].size_bytes = (uint64_t)alloc->SizeBytes;
                    tmpEntries[entryCount].reserved0 = 0;

                    entryCount += 1;
                    break;
                }

                if (existing == allocId) {
                    /* Duplicate alloc_id; keep the first GPA observed. */
                    break;
                }

                slot = (slot + 1) & mask;
            }
        }
    }

    const SIZE_T sizeBytes = sizeof(struct aerogpu_alloc_table_header) + ((SIZE_T)entryCount * sizeof(struct aerogpu_alloc_entry));
    if (sizeBytes > UINT32_MAX) {
        if (seen) {
            ExFreePoolWithTag(seen, AEROGPU_POOL_TAG);
        }
        if (tmpEntries) {
            ExFreePoolWithTag(tmpEntries, AEROGPU_POOL_TAG);
        }
        return STATUS_INTEGER_OVERFLOW;
    }

    PHYSICAL_ADDRESS pa;
    PVOID va = AeroGpuAllocContiguous(sizeBytes, &pa);
    if (!va) {
        if (seen) {
            ExFreePoolWithTag(seen, AEROGPU_POOL_TAG);
        }
        if (tmpEntries) {
            ExFreePoolWithTag(tmpEntries, AEROGPU_POOL_TAG);
        }
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    struct aerogpu_alloc_table_header* hdr = (struct aerogpu_alloc_table_header*)va;
    hdr->magic = AEROGPU_ALLOC_TABLE_MAGIC;
    hdr->abi_version = AEROGPU_ABI_VERSION_U32;
    hdr->size_bytes = (uint32_t)sizeBytes;
    hdr->entry_count = (uint32_t)entryCount;
    hdr->entry_stride_bytes = (uint32_t)sizeof(struct aerogpu_alloc_entry);
    hdr->reserved0 = 0;

    if (entryCount) {
        struct aerogpu_alloc_entry* outEntries = (struct aerogpu_alloc_entry*)(hdr + 1);
        RtlCopyMemory(outEntries, tmpEntries, (SIZE_T)entryCount * sizeof(*outEntries));
    }

    if (seen) {
        ExFreePoolWithTag(seen, AEROGPU_POOL_TAG);
    }
    if (tmpEntries) {
        ExFreePoolWithTag(tmpEntries, AEROGPU_POOL_TAG);
    }

    *OutVa = va;
    *OutPa = pa;
    *OutSizeBytes = (UINT)sizeBytes;
    return STATUS_SUCCESS;
}

static VOID AeroGpuProgramScanout(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ PHYSICAL_ADDRESS FbPa)
{
    const ULONG enable = Adapter->SourceVisible ? 1u : 0u;

    if (Adapter->UsingNewAbi) {
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_WIDTH, Adapter->CurrentWidth);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_HEIGHT, Adapter->CurrentHeight);
        AeroGpuWriteRegU32(Adapter,
                           AEROGPU_MMIO_REG_SCANOUT0_FORMAT,
                           AeroGpuLegacyScanoutFormatToNew(Adapter->CurrentFormat));
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES, Adapter->CurrentPitch);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO, FbPa.LowPart);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI, (ULONG)(FbPa.QuadPart >> 32));
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, enable);

        if (!enable) {
            /* Be robust against stale vblank IRQ state on scanout disable. */
            AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
        }
        return;
    }

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

        AeroGpuFreeContiguous(sub->AllocTableVa);
        AeroGpuFreeContiguous(sub->DmaCopyVa);
        AeroGpuFreeContiguous(sub->DescVa);
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

        AeroGpuFreeContiguous(sub->AllocTableVa);
        AeroGpuFreeContiguous(sub->DmaCopyVa);
        AeroGpuFreeContiguous(sub->DescVa);
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
    adapter->VblankPeriodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;

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
    adapter->UsingNewAbi = (magic == AEROGPU_PCI_MMIO_MAGIC) ? TRUE : FALSE;
    if (adapter->UsingNewAbi) {
        const ULONG abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ABI_VERSION);
        AEROGPU_LOG("StartDevice: MMIO magic=0x%08lx (new ABI) abi=0x%08lx", magic, abiVersion);
    } else {
        const ULONG version = AeroGpuReadRegU32(adapter, AEROGPU_REG_VERSION);
        AEROGPU_LOG("StartDevice: MMIO magic=0x%08lx (legacy) version=0x%08lx", magic, version);
    }

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
        /*
         * User-mode discovery blob used by AeroGPU UMDs (D3D9Ex/D3D10+) to
         * identify the active device ABI (legacy "ARGP" vs new "AGPU"), ABI
         * version, and feature bits.
         *
         * Backwards compatibility:
         *   - Older guest tooling expected a single ULONG return value.
         *   - Preserve that when OutputDataSize == sizeof(ULONG).
         */
        if (pQueryAdapterInfo->OutputDataSize < sizeof(ULONG)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        /*
         * v0 legacy query: return only the device ABI version.
         * - Legacy device: MMIO VERSION register (BAR0[0x0004]).
         * - New device: ABI_VERSION register (same offset).
         */
        if (pQueryAdapterInfo->OutputDataSize == sizeof(ULONG)) {
            ULONG abiVersion = 0;
            if (adapter->Bar0) {
                abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION);
            }
            *(ULONG*)pQueryAdapterInfo->pOutputData = abiVersion;
            return STATUS_SUCCESS;
        }

        if (pQueryAdapterInfo->OutputDataSize < sizeof(aerogpu_umd_private_v1)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_umd_private_v1* out = (aerogpu_umd_private_v1*)pQueryAdapterInfo->pOutputData;
        RtlZeroMemory(out, sizeof(*out));

        out->size_bytes = sizeof(*out);
        out->struct_version = AEROGPU_UMDPRIV_STRUCT_VERSION_V1;

        ULONG magic = 0;
        ULONG abiVersion = 0;
        ULONGLONG features = 0;

        if (adapter->Bar0) {
            magic = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_MAGIC);
            abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION);
            if (magic == AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
                const ULONG lo = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO);
                const ULONG hi = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI);
                features = ((ULONGLONG)hi << 32) | (ULONGLONG)lo;
            }
        }

        out->device_mmio_magic = magic;
        out->device_abi_version_u32 = abiVersion;
        out->device_features = features;

        ULONG flags = 0;
        if (magic == AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP) {
            flags |= AEROGPU_UMDPRIV_FLAG_IS_LEGACY;
        }
        if (features & AEROGPU_UMDPRIV_FEATURE_VBLANK) {
            flags |= AEROGPU_UMDPRIV_FLAG_HAS_VBLANK;
        }
        if (features & AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE) {
            flags |= AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE;
        }
        out->flags = flags;

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
    if (adapter->UsingNewAbi) {
        const ULONG enable = adapter->SourceVisible ? 1u : 0u;
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, enable);
        if (!enable) {
            /* Be robust against stale vblank IRQ state on scanout disable. */
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
        }
    } else {
        AeroGpuWriteRegU32(adapter, AEROGPU_REG_SCANOUT_ENABLE, adapter->SourceVisible ? 1u : 0u);
    }
    return STATUS_SUCCESS;
}

static __forceinline ULONGLONG AeroGpuAtomicReadU64(_In_ volatile ULONGLONG* Value)
{
    return (ULONGLONG)InterlockedCompareExchange64((volatile LONGLONG*)Value, 0, 0);
}

static __forceinline VOID AeroGpuAtomicWriteU64(_Inout_ volatile ULONGLONG* Value, _In_ ULONGLONG NewValue)
{
    InterlockedExchange64((volatile LONGLONG*)Value, (LONGLONG)NewValue);
}

static NTSTATUS APIENTRY AeroGpuDdiGetScanLine(_In_ const HANDLE hAdapter, _Inout_ DXGKARG_GETSCANLINE* pGetScanLine)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pGetScanLine) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pGetScanLine->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    const ULONG height = adapter->CurrentHeight ? adapter->CurrentHeight : 1u;
    ULONG vblankLines = height / 20;
    if (vblankLines < 10) {
        vblankLines = 10;
    }

    const ULONG totalLines = height + vblankLines;

    const ULONGLONG now100ns = KeQueryInterruptTime();
    ULONGLONG periodNs = adapter->VblankPeriodNs ? (ULONGLONG)adapter->VblankPeriodNs : (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
    ULONGLONG posNs = 0;

    BOOLEAN hasVblankRegs = FALSE;
    if (adapter->Bar0) {
        const ULONGLONG features = (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                                   ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);
        hasVblankRegs = (features & AEROGPU_FEATURE_VBLANK) != 0;
    }

    if (hasVblankRegs && adapter->Bar0) {
        const ULONG mmioPeriod = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
        if (mmioPeriod != 0) {
            adapter->VblankPeriodNs = mmioPeriod;
            periodNs = (ULONGLONG)mmioPeriod;
        } else {
            periodNs = (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
        }

        const ULONGLONG seq = (ULONGLONG)AeroGpuReadRegU64HiLoHi(adapter,
                                                                 AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                                 AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);

        const ULONGLONG cachedSeq = AeroGpuAtomicReadU64(&adapter->LastVblankSeq);
        if (seq != cachedSeq) {
            AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
            AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, now100ns);
        }

        ULONGLONG lastVblank100ns = AeroGpuAtomicReadU64(&adapter->LastVblankInterruptTime100ns);
        if (lastVblank100ns == 0) {
            /* First observation: anchor the cadence to "now". */
            AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
            AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, now100ns);
            lastVblank100ns = now100ns;
        }

        ULONGLONG delta100ns = (now100ns >= lastVblank100ns) ? (now100ns - lastVblank100ns) : 0;
        ULONGLONG deltaNs = delta100ns * 100ull;
        posNs = (periodNs != 0) ? (deltaNs % periodNs) : 0;
    } else {
        /*
         * Fallback path for devices without vblank timing registers:
         * simulate a fixed 60Hz cadence from KeQueryInterruptTime().
         */
        const ULONGLONG nowNs = now100ns * 100ull;
        if (periodNs == 0) {
            periodNs = (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
        }
        posNs = nowNs % periodNs;
    }

    ULONGLONG line = 0;
    if (periodNs != 0 && totalLines != 0) {
        line = (posNs * (ULONGLONG)totalLines) / periodNs;
        if (line >= (ULONGLONG)totalLines) {
            line = (ULONGLONG)totalLines - 1;
        }
    }

    pGetScanLine->InVerticalBlank = (line >= (ULONGLONG)height) ? TRUE : FALSE;
    pGetScanLine->ScanLine = (ULONG)line;

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

    /*
     * WDDM alloc lifetime model used by this driver:
     * - DxgkDdiCreateAllocation allocates an AEROGPU_ALLOCATION wrapper per hAllocation.
     * - DxgkDdiOpenAllocation allocates a *separate* wrapper for opened shared handles.
     * - Windows 7 dxgkrnl is expected to call:
     *     * DxgkDdiDestroyAllocation for CreateAllocation handles.
     *     * DxgkDdiCloseAllocation for OpenAllocation handles.
     *
     * CloseAllocation defensively ignores non-opened handles to avoid double-free
     * if dxgkrnl ever sends them (observed behavior should not).
     */
    BOOLEAN isShared = FALSE;
    /*
     * WDK 7.1 exposes DXGKARG_CREATEALLOCATION::Flags.CreateShared for shared
     * handle creation (DWM redirected surfaces).
     */
    isShared = pCreate->Flags.CreateShared ? TRUE : FALSE;

    static LONG g_CreateAllocPrivWarned = 0;

    for (UINT i = 0; i < pCreate->NumAllocations; ++i) {
        DXGK_ALLOCATIONINFO* info = &pCreate->pAllocationInfo[i];

        AEROGPU_ALLOCATION* alloc =
            (AEROGPU_ALLOCATION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*alloc), AEROGPU_POOL_TAG);
        if (!alloc) {
            /* Roll back allocations already created in this call. */
            for (UINT j = 0; j < i; ++j) {
                HANDLE hAllocation = pCreate->pAllocationInfo[j].hAllocation;
                if (hAllocation) {
                    ExFreePoolWithTag((PVOID)hAllocation, AEROGPU_POOL_TAG);
                    pCreate->pAllocationInfo[j].hAllocation = NULL;
                }
            }
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        ULONG allocId = (ULONG)InterlockedIncrement((volatile LONG*)&adapter->NextAllocationId);
        if (allocId == 0) {
            AEROGPU_LOG("CreateAllocation: allocation id overflow (wrapped to 0), failing with 0x%08lx",
                        STATUS_INTEGER_OVERFLOW);
            ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
            /* Roll back allocations already created in this call. */
            for (UINT j = 0; j < i; ++j) {
                HANDLE hAllocation = pCreate->pAllocationInfo[j].hAllocation;
                if (hAllocation) {
                    ExFreePoolWithTag((PVOID)hAllocation, AEROGPU_POOL_TAG);
                    pCreate->pAllocationInfo[j].hAllocation = NULL;
                }
            }
            return STATUS_INTEGER_OVERFLOW;
        }

        alloc->AllocationId = allocId;
        alloc->ShareToken = isShared ? (ULONGLONG)allocId : 0ull;
        alloc->SizeBytes = info->Size;
        alloc->Flags = (isShared ? AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED : 0);
        alloc->LastKnownPa.QuadPart = 0;

        if (info->pPrivateDriverData && info->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_priv)) {
            aerogpu_wddm_alloc_priv* priv = (aerogpu_wddm_alloc_priv*)info->pPrivateDriverData;
            RtlZeroMemory(priv, sizeof(*priv));
            priv->magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
            priv->version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
            priv->alloc_id = alloc->AllocationId;
            priv->flags = (aerogpu_u32)(alloc->Flags & ~AEROGPU_KMD_ALLOC_FLAG_OPENED);
            priv->share_token = (aerogpu_u64)alloc->ShareToken;
            priv->size_bytes = (aerogpu_u64)alloc->SizeBytes;
            priv->reserved0 = 0;
        } else {
            /*
             * Keep bring-up compatible with older UMDs that don't allocate a
             * private-data buffer yet. The stable alloc_id will be unavailable
             * to those UMDs, but allocation creation should still succeed.
             */
            if (InterlockedExchange(&g_CreateAllocPrivWarned, 1) == 0) {
                const ULONG sz = info->pPrivateDriverData ? (ULONG)info->PrivateDriverDataSize : 0u;
                AEROGPU_LOG("CreateAllocation: private data missing/too small (have=%lu need=%Iu) - alloc_id not returned",
                           sz,
                           sizeof(aerogpu_wddm_alloc_priv));
            }
        }

        info->hAllocation = (HANDLE)alloc;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        info->SupportedReadSegmentSet = 1;
        info->SupportedWriteSegmentSet = 1;

        AEROGPU_LOG("CreateAllocation: alloc_id=%lu shared=%lu share_token=0x%I64x size=%Iu",
                   alloc->AllocationId,
                   isShared ? 1ul : 0ul,
                   alloc->ShareToken,
                   alloc->SizeBytes);
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
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pOpen || !pOpen->pOpenAllocation) {
        return STATUS_INVALID_PARAMETER;
    }

    NTSTATUS st = STATUS_SUCCESS;

    for (UINT i = 0; i < pOpen->NumAllocations; ++i) {
        DXGK_OPENALLOCATIONINFO* info = &pOpen->pOpenAllocation[i];

        if (!info->pPrivateDriverData || info->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_priv)) {
            AEROGPU_LOG("OpenAllocation: missing/too small private data (have=%lu need=%Iu)",
                       (ULONG)info->PrivateDriverDataSize,
                       sizeof(aerogpu_wddm_alloc_priv));
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        const aerogpu_wddm_alloc_priv* priv = (const aerogpu_wddm_alloc_priv*)info->pPrivateDriverData;
        if (priv->magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC || priv->version != AEROGPU_WDDM_ALLOC_PRIV_VERSION ||
            priv->alloc_id == 0) {
            AEROGPU_LOG("OpenAllocation: invalid private data (magic=0x%08lx version=%lu alloc_id=%lu)",
                       (ULONG)priv->magic,
                       (ULONG)priv->version,
                       (ULONG)priv->alloc_id);
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        if (priv->size_bytes > (aerogpu_u64)(SIZE_T)(~(SIZE_T)0)) {
            AEROGPU_LOG("OpenAllocation: size overflow (alloc_id=%lu size_bytes=%I64u)",
                       (ULONG)priv->alloc_id,
                       (ULONGLONG)priv->size_bytes);
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        AEROGPU_ALLOCATION* alloc =
            (AEROGPU_ALLOCATION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*alloc), AEROGPU_POOL_TAG);
        if (!alloc) {
            st = STATUS_INSUFFICIENT_RESOURCES;
            goto Cleanup;
        }

        RtlZeroMemory(alloc, sizeof(*alloc));
        alloc->AllocationId = (ULONG)priv->alloc_id;
        alloc->ShareToken = (ULONGLONG)priv->share_token;
        alloc->SizeBytes = (SIZE_T)priv->size_bytes;
        alloc->Flags = ((ULONG)priv->flags) | AEROGPU_KMD_ALLOC_FLAG_OPENED;
        alloc->LastKnownPa.QuadPart = 0;

        info->hAllocation = (HANDLE)alloc;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        info->SupportedReadSegmentSet = 1;
        info->SupportedWriteSegmentSet = 1;

        AEROGPU_LOG("OpenAllocation: alloc_id=%lu share_token=0x%I64x size=%Iu",
                   alloc->AllocationId,
                   alloc->ShareToken,
                   alloc->SizeBytes);
    }

    return STATUS_SUCCESS;

Cleanup:
    for (UINT j = 0; j < pOpen->NumAllocations; ++j) {
        HANDLE hAllocation = pOpen->pOpenAllocation[j].hAllocation;
        if (hAllocation) {
            ExFreePoolWithTag((PVOID)hAllocation, AEROGPU_POOL_TAG);
            pOpen->pOpenAllocation[j].hAllocation = NULL;
        }
    }
    return st;
}

static NTSTATUS APIENTRY AeroGpuDdiCloseAllocation(_In_ const HANDLE hAdapter,
                                                   _In_ const DXGKARG_CLOSEALLOCATION* pClose)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pClose) {
        return STATUS_INVALID_PARAMETER;
    }

    for (UINT i = 0; i < pClose->NumAllocations; ++i) {
        HANDLE hAllocation = pClose->pAllocationList[i].hAllocation;
        if (!hAllocation) {
            continue;
        }

        const AEROGPU_ALLOCATION* alloc = (const AEROGPU_ALLOCATION*)hAllocation;
        if ((alloc->Flags & AEROGPU_KMD_ALLOC_FLAG_OPENED) == 0) {
            /*
             * Defensive: only free wrappers produced by DxgkDdiOpenAllocation.
             * CreateAllocation wrappers are owned by DxgkDdiDestroyAllocation.
             */
            continue;
        }

        ExFreePoolWithTag((PVOID)hAllocation, AEROGPU_POOL_TAG);
    }

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

static NTSTATUS APIENTRY AeroGpuBuildAndAttachMeta(_In_ UINT AllocationCount,
                                                  _In_reads_opt_(AllocationCount) const DXGK_ALLOCATIONLIST* AllocationList,
                                                  _Out_ AEROGPU_SUBMISSION_META** MetaOut)
{
    *MetaOut = NULL;

    AEROGPU_SUBMISSION_META* meta = (AEROGPU_SUBMISSION_META*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*meta), AEROGPU_POOL_TAG);
    if (!meta) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(meta, sizeof(*meta));

    NTSTATUS st =
        AeroGpuBuildAllocTable(AllocationList, AllocationCount, &meta->AllocTableVa, &meta->AllocTablePa, &meta->AllocTableSizeBytes);
    if (!NT_SUCCESS(st)) {
        ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
        return st;
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
        NTSTATUS st = AeroGpuBuildAndAttachMeta(pRender->AllocationListSize,
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
        NTSTATUS st = AeroGpuBuildAndAttachMeta(pPresent->AllocationListSize,
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
        AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pSubmitCommand->pDmaBufferPrivateData;
        type = priv->Type;
        meta = priv->Meta;
        priv->Meta = NULL;
    }

    PHYSICAL_ADDRESS dmaPa;
    PVOID dmaVa = AeroGpuAllocContiguous(pSubmitCommand->DmaBufferSize, &dmaPa);
    if (!dmaVa) {
        AeroGpuFreeSubmissionMeta(meta);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlCopyMemory(dmaVa, pSubmitCommand->pDmaBuffer, pSubmitCommand->DmaBufferSize);

    PHYSICAL_ADDRESS descPa;
    const SIZE_T descSize = sizeof(struct aerogpu_submit_desc);
    struct aerogpu_submit_desc* desc = (struct aerogpu_submit_desc*)AeroGpuAllocContiguous(descSize, &descPa);
    if (!desc) {
        AeroGpuFreeContiguous(dmaVa);
        AeroGpuFreeSubmissionMeta(meta);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    PVOID allocTableVa = NULL;
    PHYSICAL_ADDRESS allocTablePa;
    UINT allocTableSizeBytes = 0;
    allocTablePa.QuadPart = 0;
    if (meta) {
        allocTableVa = meta->AllocTableVa;
        allocTablePa = meta->AllocTablePa;
        allocTableSizeBytes = meta->AllocTableSizeBytes;
    }

    desc->desc_size_bytes = (uint32_t)sizeof(*desc);
    desc->flags = (type == AEROGPU_SUBMIT_PRESENT) ? AEROGPU_SUBMIT_FLAG_PRESENT : 0;
    desc->context_id = 0;
    desc->engine_id = AEROGPU_ENGINE_0;
    desc->cmd_gpa = (uint64_t)dmaPa.QuadPart;
    desc->cmd_size_bytes = (uint32_t)pSubmitCommand->DmaBufferSize;
    desc->cmd_reserved0 = 0;
    desc->alloc_table_gpa = allocTableSizeBytes ? (uint64_t)allocTablePa.QuadPart : 0;
    desc->alloc_table_size_bytes = (uint32_t)allocTableSizeBytes;
    desc->alloc_table_reserved0 = 0;
    desc->signal_fence = (uint64_t)fence;
    desc->reserved0 = 0;

    NTSTATUS ringSt = AeroGpuRingPushSubmit(adapter, fence, (ULONG)descSize, descPa);
    if (!NT_SUCCESS(ringSt)) {
        AeroGpuFreeContiguous(desc);
        AeroGpuFreeContiguous(dmaVa);
        AeroGpuFreeSubmissionMeta(meta);
        return ringSt;
    }

    AEROGPU_SUBMISSION* sub =
        (AEROGPU_SUBMISSION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*sub), AEROGPU_POOL_TAG);
    if (!sub) {
        /*
         * Submission already sent; keep resources around until reset/stop to
         * avoid use-after-free if the device consumes them later.
         */
        if (meta) {
            ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
        }
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
    sub->AllocTableVa = allocTableVa;
    sub->AllocTablePa = allocTablePa;
    sub->AllocTableSizeBytes = allocTableSizeBytes;

    if (meta) {
        ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
    }

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

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_FENCE) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_fence_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        const ULONG completedFence = adapter->Bar0 ? AeroGpuReadRegU32(adapter, AEROGPU_REG_FENCE_COMPLETED)
                                                   : adapter->LastCompletedFence;

        aerogpu_escape_query_fence_out* out = (aerogpu_escape_query_fence_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;
        out->last_submitted_fence = (aerogpu_u64)adapter->LastSubmittedFence;
        out->last_completed_fence = (aerogpu_u64)completedFence;
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_DUMP_RING) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_dump_ring_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_dump_ring_inout* io = (aerogpu_escape_dump_ring_inout*)pEscape->pPrivateDriverData;

        /* Only ring 0 is currently implemented. */
        if (io->ring_id != 0) {
            return STATUS_NOT_SUPPORTED;
        }

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;
        io->ring_size_bytes = adapter->RingEntryCount ? (ULONG)(adapter->RingEntryCount * sizeof(aerogpu_ring_entry)) : 0;

        io->desc_capacity = (io->desc_capacity > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS)
                                ? AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS
                                : io->desc_capacity;

        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

        const ULONG head = adapter->Bar0 ? AeroGpuReadRegU32(adapter, AEROGPU_REG_RING_HEAD) : 0;
        const ULONG tail = adapter->Bar0 ? AeroGpuReadRegU32(adapter, AEROGPU_REG_RING_TAIL) : adapter->RingTail;
        io->head = head;
        io->tail = tail;

        ULONG pending = 0;
        if (adapter->RingEntryCount != 0) {
            if (tail >= head) {
                pending = tail - head;
            } else {
                pending = tail + adapter->RingEntryCount - head;
            }
        }

        ULONG outCount = pending;
        if (outCount > io->desc_capacity) {
            outCount = io->desc_capacity;
        }
        io->desc_count = outCount;

        RtlZeroMemory(io->desc, sizeof(io->desc));
        if (adapter->RingVa && adapter->RingEntryCount && outCount) {
            aerogpu_ring_entry* ring = (aerogpu_ring_entry*)adapter->RingVa;
            for (ULONG i = 0; i < outCount; ++i) {
                const ULONG idx = (head + i) % adapter->RingEntryCount;
                const aerogpu_ring_entry entry = ring[idx];
                if (entry.type != AEROGPU_RING_ENTRY_SUBMIT) {
                    continue;
                }
                io->desc[i].fence = (aerogpu_u64)entry.submit.fence;
                io->desc[i].desc_gpa = (aerogpu_u64)entry.submit.desc_gpa;
                io->desc[i].desc_size_bytes = entry.submit.desc_size;
                io->desc[i].flags = entry.submit.flags;
            }
        }

        KeReleaseSpinLock(&adapter->RingLock, oldIrql);
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_SELFTEST) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_selftest_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_selftest_inout* io = (aerogpu_escape_selftest_inout*)pEscape->pPrivateDriverData;
        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_SELFTEST;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;
        io->passed = 0;
        io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
        io->reserved0 = 0;

        if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
            return STATUS_SUCCESS;
        }

        ULONG timeoutMs = io->timeout_ms ? io->timeout_ms : 2000u;
        if (timeoutMs > 30000u) {
            timeoutMs = 30000u;
        }

        if (!adapter->Bar0 || !adapter->RingVa || adapter->RingEntryCount == 0) {
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY;
            return STATUS_SUCCESS;
        }

        /*
         * Submit a "no-op" entry using the current completed fence value so we
         * don't advance the device fence beyond what dxgkrnl has issued.
         *
         * Completion is detected by observing ring head advancement, not fence
         * advancement.
         */
        const ULONG completedFence = AeroGpuReadRegU32(adapter, AEROGPU_REG_FENCE_COMPLETED);
        const ULONG fenceNoop = completedFence;

        AEROGPU_CMD_HEADER cmdHdr;
        RtlZeroMemory(&cmdHdr, sizeof(cmdHdr));
        cmdHdr.opcode = AEROGPU_CMD_SIGNAL_FENCE;
        cmdHdr.size_bytes = sizeof(AEROGPU_CMD_HEADER) + sizeof(AEROGPU_CMD_SIGNAL_FENCE_PAYLOAD);

        AEROGPU_CMD_SIGNAL_FENCE_PAYLOAD cmdPayload;
        RtlZeroMemory(&cmdPayload, sizeof(cmdPayload));
        cmdPayload.fence_value = (aerogpu_u64)fenceNoop;

        const ULONG dmaSize = (ULONG)(sizeof(cmdHdr) + sizeof(cmdPayload));

        PHYSICAL_ADDRESS dmaPa;
        PVOID dmaVa = AeroGpuAllocContiguous(dmaSize, &dmaPa);
        if (!dmaVa) {
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES;
            return STATUS_SUCCESS;
        }
        RtlCopyMemory(dmaVa, &cmdHdr, sizeof(cmdHdr));
        RtlCopyMemory((PUCHAR)dmaVa + sizeof(cmdHdr), &cmdPayload, sizeof(cmdPayload));

        PHYSICAL_ADDRESS descPa;
        struct aerogpu_submit_desc* desc = (struct aerogpu_submit_desc*)AeroGpuAllocContiguous(sizeof(*desc), &descPa);
        if (!desc) {
            AeroGpuFreeContiguous(dmaVa);
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES;
            return STATUS_SUCCESS;
        }

        desc->desc_size_bytes = (uint32_t)sizeof(*desc);
        desc->flags = 0;
        desc->context_id = 0;
        desc->engine_id = AEROGPU_ENGINE_0;
        desc->cmd_gpa = (uint64_t)dmaPa.QuadPart;
        desc->cmd_size_bytes = (uint32_t)dmaSize;
        desc->cmd_reserved0 = 0;
        desc->alloc_table_gpa = 0;
        desc->alloc_table_size_bytes = 0;
        desc->alloc_table_reserved0 = 0;
        desc->signal_fence = (uint64_t)fenceNoop;
        desc->reserved0 = 0;

        /* Push directly to the ring under RingLock for determinism. */
        ULONG headBefore = 0;
        NTSTATUS pushStatus = STATUS_SUCCESS;
        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

            /* Require an idle GPU to avoid perturbing dxgkrnl's fence tracking. */
            {
                KIRQL pendingIrql;
                KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);
                BOOLEAN busy = !IsListEmpty(&adapter->PendingSubmissions) ||
                               (adapter->LastSubmittedFence != completedFence);
                KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
                if (busy) {
                    pushStatus = STATUS_DEVICE_BUSY;
                }
            }

            ULONG head = AeroGpuReadRegU32(adapter, AEROGPU_REG_RING_HEAD);
            ULONG tail = adapter->RingTail;
            headBefore = head;

            if (NT_SUCCESS(pushStatus) && head != tail) {
                pushStatus = STATUS_DEVICE_BUSY;
            }

            ULONG nextTail = (adapter->RingTail + 1) % adapter->RingEntryCount;
            if (NT_SUCCESS(pushStatus) && nextTail == head) {
                pushStatus = STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
            } else if (NT_SUCCESS(pushStatus)) {
                aerogpu_ring_entry* ring = (aerogpu_ring_entry*)adapter->RingVa;
                ring[adapter->RingTail].submit.type = AEROGPU_RING_ENTRY_SUBMIT;
                ring[adapter->RingTail].submit.flags = 0;
                ring[adapter->RingTail].submit.fence = fenceNoop;
                ring[adapter->RingTail].submit.desc_size = (ULONG)sizeof(*desc);
                ring[adapter->RingTail].submit.desc_gpa = (aerogpu_u64)descPa.QuadPart;

                KeMemoryBarrier();
                adapter->RingTail = nextTail;
                AeroGpuWriteRegU32(adapter, AEROGPU_REG_RING_TAIL, adapter->RingTail);
                AeroGpuWriteRegU32(adapter, AEROGPU_REG_RING_DOORBELL, 1);
            }

            KeReleaseSpinLock(&adapter->RingLock, oldIrql);
        }

        if (!NT_SUCCESS(pushStatus)) {
            AeroGpuFreeContiguous(desc);
            AeroGpuFreeContiguous(dmaVa);
            io->error_code = (pushStatus == STATUS_DEVICE_BUSY)
                                 ? AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY
                                 : AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY;
            return STATUS_SUCCESS;
        }

        /* Poll for ring head advancement. */
        ULONGLONG start = KeQueryInterruptTime();
        ULONGLONG deadline = start + ((ULONGLONG)timeoutMs * 10000ull);
        NTSTATUS testStatus = STATUS_TIMEOUT;
        while (KeQueryInterruptTime() < deadline) {
            ULONG headNow = AeroGpuReadRegU32(adapter, AEROGPU_REG_RING_HEAD);
            if (headNow != headBefore) {
                testStatus = STATUS_SUCCESS;
                break;
            }

            LARGE_INTEGER interval;
            interval.QuadPart = -10000; /* 1ms */
            KeDelayExecutionThread(KernelMode, FALSE, &interval);
        }

        if (NT_SUCCESS(testStatus)) {
            AeroGpuFreeContiguous(desc);
            AeroGpuFreeContiguous(dmaVa);
            io->passed = 1;
            io->error_code = AEROGPU_DBGCTL_SELFTEST_OK;
        } else {
            /*
             * The device did not consume the entry in time. Do not free the
             * descriptor/DMA buffer to avoid use-after-free if the device
             * consumes it later.
             */
            io->passed = 0;
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT;
        }

        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_DUMP_VBLANK) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_dump_vblank_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        if (!adapter->Bar0) {
            return STATUS_DEVICE_NOT_READY;
        }

        aerogpu_escape_dump_vblank_inout* io = (aerogpu_escape_dump_vblank_inout*)pEscape->pPrivateDriverData;

        /* Only VidPn source 0 is currently implemented. */
        if (io->vidpn_source_id != AEROGPU_VIDPN_SOURCE_ID) {
            return STATUS_NOT_SUPPORTED;
        }

        const aerogpu_u32 featuresLo = (aerogpu_u32)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO);
        const aerogpu_u32 featuresHi = (aerogpu_u32)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI);
        const aerogpu_u64 features = ((aerogpu_u64)featuresHi << 32) | (aerogpu_u64)featuresLo;
        if ((features & (aerogpu_u64)AEROGPU_FEATURE_VBLANK) == 0) {
            return STATUS_NOT_SUPPORTED;
        }

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_DUMP_VBLANK;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;

        io->irq_status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
        io->irq_enable = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE);
        io->flags = AEROGPU_DBGCTL_VBLANK_SUPPORTED;

        io->vblank_seq = AeroGpuReadRegU64HiLoHi(adapter,
                                                 AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                 AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
        io->last_vblank_time_ns = AeroGpuReadRegU64HiLoHi(adapter,
                                                          AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
                                                          AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
        io->vblank_period_ns = (aerogpu_u32)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
        io->reserved0 = 0;
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
    init.DxgkDdiGetScanLine = AeroGpuDdiGetScanLine;

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
