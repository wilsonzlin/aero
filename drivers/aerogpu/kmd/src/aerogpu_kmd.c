#include "aerogpu_ring.h"

#include "aerogpu_kmd.h"
#include "aerogpu_dbgctl_escape.h"
#include "aerogpu_cmd.h"
#include "aerogpu_umd_private.h"
#include "aerogpu_wddm_alloc.h"
#include "aerogpu_win7_abi.h"

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

/* ---- WDDM interrupt type plumbing --------------------------------------- */

/*
 * Dxgkrnl tells the miniport which DXGK_INTERRUPT_TYPE it expects for vblank
 * delivery via DxgkDdiControlInterrupt. Record that type at runtime and use it
 * when notifying vblank interrupts from the ISR. This avoids hard-coding the
 * enum constant name and union member layout across WDK variants.
 */

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

static VOID AeroGpuFreeSubmissionMeta(_In_opt_ AEROGPU_SUBMISSION_META* Meta);

static NTSTATUS AeroGpuMetaHandleStore(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ AEROGPU_SUBMISSION_META* Meta, _Out_ ULONGLONG* HandleOut)
{
    *HandleOut = 0;

    AEROGPU_META_HANDLE_ENTRY* entry =
        (AEROGPU_META_HANDLE_ENTRY*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*entry), AEROGPU_POOL_TAG);
    if (!entry) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(entry, sizeof(*entry));
    entry->Meta = Meta;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);

    /* 0 is reserved to mean "no meta". */
    ULONGLONG handle = ++Adapter->NextMetaHandle;
    if (handle == 0) {
        handle = ++Adapter->NextMetaHandle;
    }

    entry->Handle = handle;
    InsertTailList(&Adapter->PendingMetaHandles, &entry->ListEntry);

    KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);

    *HandleOut = handle;
    return STATUS_SUCCESS;
}

static AEROGPU_SUBMISSION_META* AeroGpuMetaHandleTake(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONGLONG Handle)
{
    if (Handle == 0) {
        return NULL;
    }

    AEROGPU_META_HANDLE_ENTRY* found = NULL;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);

    for (PLIST_ENTRY it = Adapter->PendingMetaHandles.Flink; it != &Adapter->PendingMetaHandles; it = it->Flink) {
        AEROGPU_META_HANDLE_ENTRY* entry = CONTAINING_RECORD(it, AEROGPU_META_HANDLE_ENTRY, ListEntry);
        if (entry->Handle == Handle) {
            found = entry;
            RemoveEntryList(&entry->ListEntry);
            break;
        }
    }

    KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);

    if (!found) {
        return NULL;
    }

    AEROGPU_SUBMISSION_META* meta = found->Meta;
    ExFreePoolWithTag(found, AEROGPU_POOL_TAG);
    return meta;
}

static VOID AeroGpuMetaHandleFreeAll(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    for (;;) {
        AEROGPU_META_HANDLE_ENTRY* entry = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);
        if (!IsListEmpty(&Adapter->PendingMetaHandles)) {
            PLIST_ENTRY le = RemoveHeadList(&Adapter->PendingMetaHandles);
            entry = CONTAINING_RECORD(le, AEROGPU_META_HANDLE_ENTRY, ListEntry);
        }
        KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);

        if (!entry) {
            break;
        }

        AeroGpuFreeSubmissionMeta(entry->Meta);
        ExFreePoolWithTag(entry, AEROGPU_POOL_TAG);
    }
}

/* ---- Helpers ------------------------------------------------------------ */

/*
 * Read a 64-bit MMIO value exposed as two 32-bit registers in LO/HI form.
 *
 * Use an HI/LO/HI pattern to avoid tearing if the device updates the value
 * concurrently.
 */
static ULONGLONG AeroGpuReadRegU64HiLoHi(_In_ const AEROGPU_ADAPTER* Adapter, _In_ ULONG LoOffset, _In_ ULONG HiOffset)
{
    ULONG hi = AeroGpuReadRegU32(Adapter, HiOffset);
    for (;;) {
        const ULONG lo = AeroGpuReadRegU32(Adapter, LoOffset);
        const ULONG hi2 = AeroGpuReadRegU32(Adapter, HiOffset);
        if (hi == hi2) {
            return ((ULONGLONG)hi << 32) | (ULONGLONG)lo;
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
    uint64_t* seenGpa = NULL;
    uint64_t* seenSize = NULL;
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

        const SIZE_T seenGpaBytes = (SIZE_T)cap * sizeof(*seenGpa);
        seenGpa = (uint64_t*)ExAllocatePoolWithTag(NonPagedPool, seenGpaBytes, AEROGPU_POOL_TAG);
        if (!seenGpa) {
            ExFreePoolWithTag(seen, AEROGPU_POOL_TAG);
            ExFreePoolWithTag(tmpEntries, AEROGPU_POOL_TAG);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlZeroMemory(seenGpa, seenGpaBytes);

        const SIZE_T seenSizeBytes = (SIZE_T)cap * sizeof(*seenSize);
        seenSize = (uint64_t*)ExAllocatePoolWithTag(NonPagedPool, seenSizeBytes, AEROGPU_POOL_TAG);
        if (!seenSize) {
            ExFreePoolWithTag(seenGpa, AEROGPU_POOL_TAG);
            ExFreePoolWithTag(seen, AEROGPU_POOL_TAG);
            ExFreePoolWithTag(tmpEntries, AEROGPU_POOL_TAG);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlZeroMemory(seenSize, seenSizeBytes);

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
                    seenGpa[slot] = (uint64_t)List[i].PhysicalAddress.QuadPart;
                    seenSize[slot] = (uint64_t)alloc->SizeBytes;

                    tmpEntries[entryCount].alloc_id = allocId;
                    tmpEntries[entryCount].flags = 0;
                    tmpEntries[entryCount].gpa = (uint64_t)List[i].PhysicalAddress.QuadPart;
                    tmpEntries[entryCount].size_bytes = (uint64_t)alloc->SizeBytes;
                    tmpEntries[entryCount].reserved0 = 0;

                    entryCount += 1;
                    break;
                }

                if (existing == allocId) {
                    const uint64_t gpa = (uint64_t)List[i].PhysicalAddress.QuadPart;
                    const uint64_t sizeBytes = (uint64_t)alloc->SizeBytes;
                    if (seenGpa[slot] != gpa || seenSize[slot] != sizeBytes) {
                        AEROGPU_LOG("BuildAllocTable: alloc_id collision: alloc_id=%lu gpa0=0x%I64x size0=%I64u gpa1=0x%I64x size1=%I64u",
                                   (ULONG)allocId,
                                   (ULONGLONG)seenGpa[slot],
                                   (ULONGLONG)seenSize[slot],
                                   (ULONGLONG)gpa,
                                   (ULONGLONG)sizeBytes);
                        if (seenSize) {
                            ExFreePoolWithTag(seenSize, AEROGPU_POOL_TAG);
                        }
                        if (seenGpa) {
                            ExFreePoolWithTag(seenGpa, AEROGPU_POOL_TAG);
                        }
                        if (seen) {
                            ExFreePoolWithTag(seen, AEROGPU_POOL_TAG);
                        }
                        if (tmpEntries) {
                            ExFreePoolWithTag(tmpEntries, AEROGPU_POOL_TAG);
                        }
                        return STATUS_INVALID_PARAMETER;
                    }
                    /* Duplicate alloc_id for identical backing range; keep the first entry. */
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
        if (seenGpa) {
            ExFreePoolWithTag(seenGpa, AEROGPU_POOL_TAG);
        }
        if (seenSize) {
            ExFreePoolWithTag(seenSize, AEROGPU_POOL_TAG);
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
        if (seenGpa) {
            ExFreePoolWithTag(seenGpa, AEROGPU_POOL_TAG);
        }
        if (seenSize) {
            ExFreePoolWithTag(seenSize, AEROGPU_POOL_TAG);
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
    if (seenGpa) {
        ExFreePoolWithTag(seenGpa, AEROGPU_POOL_TAG);
    }
    if (seenSize) {
        ExFreePoolWithTag(seenSize, AEROGPU_POOL_TAG);
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

    if (Adapter->UsingNewAbi || Adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_WIDTH, Adapter->CurrentWidth);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_HEIGHT, Adapter->CurrentHeight);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FORMAT, Adapter->CurrentFormat);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES, Adapter->CurrentPitch);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO, FbPa.LowPart);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI, (ULONG)(FbPa.QuadPart >> 32));
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, enable);

        if (!enable && Adapter->SupportsVblank) {
            /* Be robust against stale vblank IRQ state on scanout disable. */
            AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
        }
        return;
    }

    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_LO, FbPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_HI, (ULONG)(FbPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_PITCH, Adapter->CurrentPitch);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_WIDTH, Adapter->CurrentWidth);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_HEIGHT, Adapter->CurrentHeight);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FORMAT, AEROGPU_LEGACY_SCANOUT_X8R8G8B8);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE, enable);
}

static VOID AeroGpuSetScanoutEnable(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONG Enable)
{
    if (!Adapter->Bar0) {
        return;
    }

    if (Adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, Enable);
        if (!Enable) {
            /* Be robust against stale vblank IRQ state on scanout disable. */
            AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
        }
    } else {
        AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE, Enable);
    }
}

static NTSTATUS AeroGpuLegacyRingInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    Adapter->RingEntryCount = AEROGPU_RING_ENTRY_COUNT_DEFAULT;
    Adapter->RingTail = 0;

    const SIZE_T ringBytes = Adapter->RingEntryCount * sizeof(aerogpu_legacy_ring_entry);
    Adapter->RingVa = AeroGpuAllocContiguous(ringBytes, &Adapter->RingPa);
    if (!Adapter->RingVa) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    Adapter->RingSizeBytes = (ULONG)ringBytes;

    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, Adapter->RingPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_BASE_HI, (ULONG)(Adapter->RingPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, Adapter->RingEntryCount);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);

    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuV1RingInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    Adapter->RingEntryCount = AEROGPU_RING_ENTRY_COUNT_DEFAULT;
    Adapter->RingTail = 0;

    SIZE_T ringBytes = sizeof(struct aerogpu_ring_header) +
                       (SIZE_T)Adapter->RingEntryCount * sizeof(struct aerogpu_submit_desc);
    ringBytes = (ringBytes + PAGE_SIZE - 1) & ~(SIZE_T)(PAGE_SIZE - 1);

    Adapter->RingVa = AeroGpuAllocContiguous(ringBytes, &Adapter->RingPa);
    if (!Adapter->RingVa) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    Adapter->RingSizeBytes = (ULONG)ringBytes;

    Adapter->RingHeader = (struct aerogpu_ring_header*)Adapter->RingVa;
    Adapter->RingHeader->magic = AEROGPU_RING_MAGIC;
    Adapter->RingHeader->abi_version = AEROGPU_ABI_VERSION_U32;
    Adapter->RingHeader->size_bytes = (uint32_t)ringBytes;
    Adapter->RingHeader->entry_count = (uint32_t)Adapter->RingEntryCount;
    Adapter->RingHeader->entry_stride_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
    Adapter->RingHeader->flags = 0;
    Adapter->RingHeader->head = 0;
    Adapter->RingHeader->tail = 0;

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_GPA_LO, Adapter->RingPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_GPA_HI, (ULONG)(Adapter->RingPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, Adapter->RingSizeBytes);
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_CONTROL, AEROGPU_RING_CONTROL_ENABLE);

    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuV1FencePageInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    Adapter->FencePageVa = (struct aerogpu_fence_page*)AeroGpuAllocContiguous(PAGE_SIZE, &Adapter->FencePagePa);
    if (!Adapter->FencePageVa) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Adapter->FencePageVa->magic = AEROGPU_FENCE_PAGE_MAGIC;
    Adapter->FencePageVa->abi_version = AEROGPU_ABI_VERSION_U32;
    Adapter->FencePageVa->completed_fence = 0;

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, Adapter->FencePagePa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, (ULONG)(Adapter->FencePagePa.QuadPart >> 32));

    return STATUS_SUCCESS;
}

static VOID AeroGpuRingCleanup(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    AeroGpuFreeContiguous(Adapter->RingVa);
    Adapter->RingVa = NULL;
    Adapter->RingPa.QuadPart = 0;
    Adapter->RingSizeBytes = 0;
    Adapter->RingEntryCount = 0;
    Adapter->RingTail = 0;
    Adapter->RingHeader = NULL;

    AeroGpuFreeContiguous(Adapter->FencePageVa);
    Adapter->FencePageVa = NULL;
    Adapter->FencePagePa.QuadPart = 0;
}

static NTSTATUS AeroGpuLegacyRingPushSubmit(_Inout_ AEROGPU_ADAPTER* Adapter,
                                            _In_ ULONG Fence,
                                            _In_ ULONG DescSize,
                                            _In_ PHYSICAL_ADDRESS DescPa)
{
    if (!Adapter->RingVa || !Adapter->Bar0) {
        return STATUS_DEVICE_NOT_READY;
    }

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->RingLock, &oldIrql);

    ULONG head = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_RING_HEAD);
    ULONG nextTail = (Adapter->RingTail + 1) % Adapter->RingEntryCount;
    if (nextTail == head) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
    }

    aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)Adapter->RingVa;
    ring[Adapter->RingTail].submit.type = AEROGPU_LEGACY_RING_ENTRY_SUBMIT;
    ring[Adapter->RingTail].submit.flags = 0;
    ring[Adapter->RingTail].submit.fence = Fence;
    ring[Adapter->RingTail].submit.desc_size = DescSize;
    ring[Adapter->RingTail].submit.desc_gpa = (uint64_t)DescPa.QuadPart;

    KeMemoryBarrier();
    Adapter->RingTail = nextTail;
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_TAIL, Adapter->RingTail);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_DOORBELL, 1);

    KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuV1RingPushSubmit(_Inout_ AEROGPU_ADAPTER* Adapter,
                                        _In_ uint32_t Flags,
                                        _In_ PHYSICAL_ADDRESS CmdPa,
                                        _In_ ULONG CmdSizeBytes,
                                        _In_ uint64_t AllocTableGpa,
                                        _In_ uint32_t AllocTableSizeBytes,
                                        _In_ ULONGLONG SignalFence)
{
    if (!Adapter->RingVa || !Adapter->RingHeader || !Adapter->Bar0 || Adapter->RingEntryCount == 0) {
        return STATUS_DEVICE_NOT_READY;
    }

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->RingLock, &oldIrql);

    const uint32_t head = Adapter->RingHeader->head;
    const uint32_t tail = Adapter->RingTail;
    const uint32_t pending = tail - head;
    if (pending >= Adapter->RingEntryCount) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
    }

    const uint32_t slot = tail & (Adapter->RingEntryCount - 1);
    struct aerogpu_submit_desc* desc =
        (struct aerogpu_submit_desc*)((PUCHAR)Adapter->RingVa + sizeof(struct aerogpu_ring_header) +
                                      ((SIZE_T)slot * sizeof(struct aerogpu_submit_desc)));

    RtlZeroMemory(desc, sizeof(*desc));
    desc->desc_size_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
    desc->flags = Flags;
    desc->context_id = 0;
    desc->engine_id = AEROGPU_ENGINE_0;
    desc->cmd_gpa = (uint64_t)CmdPa.QuadPart;
    desc->cmd_size_bytes = CmdSizeBytes;
    desc->alloc_table_gpa = AllocTableGpa;
    desc->alloc_table_size_bytes = AllocTableSizeBytes;
    desc->signal_fence = (uint64_t)SignalFence;

    KeMemoryBarrier();
    Adapter->RingTail = tail + 1;
    Adapter->RingHeader->tail = Adapter->RingTail;
    KeMemoryBarrier();

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_DOORBELL, 1);

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

static VOID AeroGpuRetireSubmissionsUpToFence(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONGLONG CompletedFence)
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

static VOID AeroGpuTrackAllocation(_Inout_ AEROGPU_ADAPTER* Adapter, _Inout_ AEROGPU_ALLOCATION* Allocation)
{
    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);
    InsertTailList(&Adapter->Allocations, &Allocation->ListEntry);
    KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);
}

static BOOLEAN AeroGpuTryUntrackAllocation(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ const AEROGPU_ALLOCATION* Allocation)
{
    BOOLEAN found = FALSE;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);

    for (PLIST_ENTRY entry = Adapter->Allocations.Flink; entry != &Adapter->Allocations; entry = entry->Flink) {
        const AEROGPU_ALLOCATION* candidate = CONTAINING_RECORD(entry, AEROGPU_ALLOCATION, ListEntry);
        if (candidate == Allocation) {
            RemoveEntryList(entry);
            found = TRUE;
            break;
        }
    }

    KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);
    return found;
}

static VOID AeroGpuUntrackAndFreeAllocation(_Inout_ AEROGPU_ADAPTER* Adapter, _In_opt_ HANDLE hAllocation)
{
    if (!hAllocation) {
        return;
    }

    AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)hAllocation;
    if (!AeroGpuTryUntrackAllocation(Adapter, alloc)) {
        /*
         * Be tolerant of dxgkrnl calling CloseAllocation/DestroyAllocation in
         * different patterns. If the handle is already freed we should not
         * touch it again.
         */
        static LONG g_UntrackedAllocFreeWarned = 0;
        if (InterlockedExchange(&g_UntrackedAllocFreeWarned, 1) == 0) {
            AEROGPU_LOG("Allocation free: untracked handle=%p", hAllocation);
        }
        return;
    }

    ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
}

static VOID AeroGpuFreeAllAllocations(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    for (;;) {
        AEROGPU_ALLOCATION* alloc = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);
        if (!IsListEmpty(&Adapter->Allocations)) {
            PLIST_ENTRY entry = RemoveHeadList(&Adapter->Allocations);
            alloc = CONTAINING_RECORD(entry, AEROGPU_ALLOCATION, ListEntry);
        }
        KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);

        if (!alloc) {
            return;
        }

        ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
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
    KeInitializeSpinLock(&adapter->IrqEnableLock);
    KeInitializeSpinLock(&adapter->PendingLock);
    InitializeListHead(&adapter->PendingSubmissions);
    KeInitializeSpinLock(&adapter->MetaHandleLock);
    InitializeListHead(&adapter->PendingMetaHandles);
    adapter->NextMetaHandle = 0;
    KeInitializeSpinLock(&adapter->AllocationsLock);
    InitializeListHead(&adapter->Allocations);

    adapter->CurrentWidth = 1024;
    adapter->CurrentHeight = 768;
    adapter->CurrentPitch = 1024 * 4;
    adapter->CurrentFormat = AEROGPU_FORMAT_B8G8R8X8_UNORM;
    adapter->SourceVisible = TRUE;
    adapter->VblankPeriodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;

    /*
     * Initialise so that the first InterlockedIncrement() yields
     * AEROGPU_WDDM_ALLOC_ID_KMD_MIN.
     */
    adapter->NextKmdAllocId = (LONG)AEROGPU_WDDM_ALLOC_ID_UMD_MAX;

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

    const ULONG magic = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_MAGIC);
    ULONGLONG v1Features = 0;

    /*
     * ABI detection: treat the versioned "AGPU" MMIO magic as the new ABI, and
     * fall back to the legacy register map otherwise.
     *
     * This keeps older emulator device models working even if they don't report
     * the expected legacy magic value.
     */
    adapter->AbiKind = AEROGPU_ABI_KIND_LEGACY;
    adapter->UsingNewAbi = FALSE;
    if (magic == AEROGPU_MMIO_MAGIC) {
        adapter->AbiKind = AEROGPU_ABI_KIND_V1;
        adapter->UsingNewAbi = TRUE;
        const ULONG abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ABI_VERSION);
        const ULONG abiMajor = abiVersion >> 16;
        if (abiMajor != AEROGPU_ABI_MAJOR) {
            AEROGPU_LOG("StartDevice: unsupported ABI major=%lu (abi=0x%08lx)", abiMajor, abiVersion);
            MmUnmapIoSpace(adapter->Bar0, adapter->Bar0Length);
            adapter->Bar0 = NULL;
            adapter->Bar0Length = 0;
            return STATUS_NOT_SUPPORTED;
        }
        const ULONGLONG features = (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                                   ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);
        v1Features = features;
        AEROGPU_LOG("StartDevice: ABI=v1 magic=0x%08lx (new) abi=0x%08lx features=0x%I64x",
                    magic,
                    abiVersion,
                    (unsigned long long)features);
    } else {
        const ULONG version = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_VERSION);
        if (magic != AEROGPU_LEGACY_MMIO_MAGIC) {
            AEROGPU_LOG("StartDevice: unknown MMIO magic=0x%08lx (expected 0x%08x); assuming legacy ABI",
                        magic,
                        AEROGPU_LEGACY_MMIO_MAGIC);
        }
        AEROGPU_LOG("StartDevice: ABI=legacy magic=0x%08lx version=0x%08lx", magic, version);
    }

    {
        ULONGLONG features = (adapter->AbiKind == AEROGPU_ABI_KIND_V1) ? v1Features : 0;

        adapter->DeviceFeatures = features;
        adapter->SupportsVblank = ((features & AEROGPU_FEATURE_VBLANK) != 0) ? TRUE : FALSE;
        adapter->VblankInterruptTypeValid = FALSE;
        adapter->VblankInterruptType = 0;
        AEROGPU_LOG("StartDevice: FEATURES=0x%I64x (vblank=%lu)", features, adapter->SupportsVblank ? 1ul : 0ul);

        InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankSeq, 0);
        InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankInterruptTime100ns, 0);
        adapter->VblankPeriodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
    }

    if (adapter->DxgkInterface.DxgkCbRegisterInterrupt) {
        NTSTATUS st = adapter->DxgkInterface.DxgkCbRegisterInterrupt(adapter->StartInfo.hDxgkHandle);
        if (!NT_SUCCESS(st)) {
            AEROGPU_LOG("StartDevice: DxgkCbRegisterInterrupt failed 0x%08lx", st);
        }
    }

    NTSTATUS ringSt = STATUS_SUCCESS;
    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        ringSt = AeroGpuV1RingInit(adapter);
        if (NT_SUCCESS(ringSt)) {
            ringSt = AeroGpuV1FencePageInit(adapter);
        }
        if (NT_SUCCESS(ringSt)) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
            {
                KIRQL oldIrql;
                KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                adapter->IrqEnableMask = AEROGPU_IRQ_FENCE | AEROGPU_IRQ_ERROR;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->IrqEnableMask);
                KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
            }
        }
    } else {
        ringSt = AeroGpuLegacyRingInit(adapter);
    }
    if (!NT_SUCCESS(ringSt)) {
        AeroGpuRingCleanup(adapter);
        MmUnmapIoSpace(adapter->Bar0, adapter->Bar0Length);
        adapter->Bar0 = NULL;
        adapter->Bar0Length = 0;
        return ringSt;
    }

    if (adapter->DxgkInterface.DxgkCbEnableInterrupt) {
        adapter->DxgkInterface.DxgkCbEnableInterrupt(adapter->StartInfo.hDxgkHandle);
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

    if (adapter->Bar0) {
        /* Stop device IRQ generation before unregistering the ISR. */
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            {
                KIRQL oldIrql;
                KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                adapter->IrqEnableMask = 0;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
                KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
            }
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
        } else {
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
        }
    }

    if (adapter->DxgkInterface.DxgkCbDisableInterrupt) {
        adapter->DxgkInterface.DxgkCbDisableInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    if (adapter->DxgkInterface.DxgkCbUnregisterInterrupt) {
        adapter->DxgkInterface.DxgkCbUnregisterInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    AeroGpuMetaHandleFreeAll(adapter);
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
    AeroGpuMetaHandleFreeAll(adapter);
    AeroGpuFreeAllAllocations(adapter);
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
        caps->DmaBufferPrivateDataSize = (ULONG)AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
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

        out->PagingBufferPrivateDataSize = (ULONG)AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
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
        ULONGLONG fencePageGpa = 0;

        if (adapter->Bar0) {
            magic = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_MAGIC);
            abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION);
            if (magic == AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
                const ULONG lo = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO);
                const ULONG hi = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI);
                features = ((ULONGLONG)hi << 32) | (ULONGLONG)lo;

                /*
                 * The UMD-private blob exposes a convenience flag indicating
                 * whether a shared fence page is configured/usable. Distinguish
                 * this from the raw feature bit (which only indicates support).
                 */
                if (features & AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE) {
                    const ULONG fenceLo = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO);
                    const ULONG fenceHi = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI);
                    fencePageGpa = ((ULONGLONG)fenceHi << 32) | (ULONGLONG)fenceLo;
                }
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
        if (fencePageGpa != 0) {
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
    AeroGpuSetScanoutEnable(adapter, adapter->SourceVisible ? 1u : 0u);
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
    ULONGLONG periodNs =
        adapter->VblankPeriodNs ? (ULONGLONG)adapter->VblankPeriodNs : (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
    ULONGLONG posNs = 0;

    if (adapter->Bar0 && adapter->AbiKind == AEROGPU_ABI_KIND_V1 && adapter->SupportsVblank) {
        const ULONG mmioPeriod = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
        if (mmioPeriod != 0) {
            adapter->VblankPeriodNs = mmioPeriod;
            periodNs = (ULONGLONG)mmioPeriod;
        } else {
            periodNs = (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
        }

        const ULONGLONG seq = AeroGpuReadRegU64HiLoHi(adapter,
                                                      AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                      AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);

        const ULONGLONG cachedSeq = AeroGpuAtomicReadU64(&adapter->LastVblankSeq);
        ULONGLONG lastVblank100ns = AeroGpuAtomicReadU64(&adapter->LastVblankInterruptTime100ns);
        if (seq != cachedSeq) {
            /*
             * We only know the vblank sequence counter and the nominal period; we don't have
             * a reliable way to translate the device's `VBLANK_TIME_NS` epoch to guest time.
             *
             * Instead, keep a guest-time estimate of the last vblank by advancing the
             * previous estimate by `deltaSeq * period`. This preserves phase information
             * even if callers poll `GetScanLine` infrequently (e.g. sleep-based loops),
             * avoiding "stuck at scanline 0" behavior.
             */
            ULONGLONG newLastVblank100ns = now100ns;

            if (lastVblank100ns != 0 && cachedSeq != 0) {
                const ULONGLONG deltaSeq = seq - cachedSeq;
                ULONGLONG advance100ns = 0;
                if (deltaSeq != 0) {
                    /*
                     * Compute (deltaSeq * periodNs) / 100 in 100ns units. Do the multiply first
                     * to preserve sub-100ns remainder across multiple vblank intervals.
                     */
                    if (periodNs == 0 || deltaSeq > (~0ull / periodNs)) {
                        advance100ns = ~0ull;
                    } else {
                        const ULONGLONG advanceNs = deltaSeq * periodNs;
                        advance100ns = advanceNs / 100ull;
                    }
                }

                ULONGLONG predicted = lastVblank100ns;
                if (advance100ns == ~0ull || predicted > (~0ull - advance100ns)) {
                    predicted = ~0ull;
                } else {
                    predicted += advance100ns;
                }

                if (predicted <= now100ns) {
                    newLastVblank100ns = predicted;
                }
            }

            AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
            AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, newLastVblank100ns);
            lastVblank100ns = newLastVblank100ns;
        }

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
     * WDDM allocation lifetime model used by this driver:
     * - Both DxgkDdiCreateAllocation and DxgkDdiOpenAllocation allocate an
     *   AEROGPU_ALLOCATION wrapper per returned hAllocation.
     * - Windows 7 may release those handles via either DxgkDdiCloseAllocation
     *   or DxgkDdiDestroyAllocation depending on the object and sharing model.
     *
     * To avoid double-free/use-after-free across different Win7 call patterns,
     * the driver tracks all live wrappers in adapter->Allocations and only frees
     * handles that are still tracked.
     */
    /*
     * On Windows 7/WDDM 1.1, DXGKARG_CREATEALLOCATION::Flags.CreateShared is used for shared
     * handle creation (notably DWM redirected surfaces).
     */
    const BOOLEAN isShared = pCreate->Flags.CreateShared ? TRUE : FALSE;

#if DBG
    BOOLEAN logCall = FALSE;
    /*
     * WDDM resources may be represented as multiple allocations (mips/arrays/planes).
     *
     * AeroGPU's MVP shared-surface interop assumes a single backing allocation, so
     * we log shared/multi-allocation creation requests to characterize real-world
     * behavior (notably DWM redirected surfaces) and to aid bring-up debugging.
     *
     * Guard + rate-limit to avoid excessive DbgPrint spam in hot paths.
     */
    {
        const BOOLEAN interesting = isShared || (pCreate->NumAllocations != 1);
        if (interesting) {
            enum { kLogLimit = 64 };
            static LONG s_logCount = 0;
            const LONG n = InterlockedIncrement(&s_logCount);
            if (n <= kLogLimit) {
                logCall = TRUE;
                AEROGPU_LOG("CreateAllocation: NumAllocations=%u CreateShared=%u Flags=0x%08X",
                            (unsigned)pCreate->NumAllocations,
                            (unsigned)isShared,
                            (unsigned)pCreate->Flags.Value);

                for (UINT i = 0; i < pCreate->NumAllocations; ++i) {
                    const DXGK_ALLOCATIONINFO* info = &pCreate->pAllocationInfo[i];
                    AEROGPU_LOG("  alloc[%u]: Size=%Iu", (unsigned)i, info->Size);
                }
            } else if (n == (kLogLimit + 1)) {
                AEROGPU_LOG0("CreateAllocation: log limit reached; suppressing further messages");
            }
        }
    }
#endif

    /*
     * MVP restriction: shared resources must be represented as a single allocation.
     *
     * The guest↔host shared-surface protocol currently only supports one backing
     * allocation per share token. Enforce this invariant in KMD to ensure we fail
     * predictably (rather than corrupting host-side shared-surface tables) if an
     * API attempts to share a resource that would require multiple allocations.
     */
    if (isShared && pCreate->NumAllocations != 1) {
#if DBG
        AEROGPU_LOG("CreateAllocation: rejecting shared resource with NumAllocations=%u (MVP supports only single-allocation shared surfaces)",
                    (unsigned)pCreate->NumAllocations);
#endif
        return STATUS_NOT_SUPPORTED;
    }

    for (UINT i = 0; i < pCreate->NumAllocations; ++i) {
        DXGK_ALLOCATIONINFO* info = &pCreate->pAllocationInfo[i];

        ULONG allocId = 0;
        ULONGLONG shareToken = 0;

        /*
         * WDDM allocation private driver data (if provided).
         *
         * This is an INPUT buffer (UMD->KMD). For standard allocations created
         * by dxgkrnl (for example primary surfaces), the runtime may not provide
         * an AeroGPU private-data blob; in that case we synthesize an internal
         * alloc_id from a reserved namespace.
         */
        if (info->pPrivateDriverData && info->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_private_data)) {
            const aerogpu_wddm_alloc_private_data* priv =
                (const aerogpu_wddm_alloc_private_data*)info->pPrivateDriverData;

            if (priv->magic == AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC) {
                if (priv->version != AEROGPU_WDDM_ALLOC_PRIVATE_DATA_VERSION || priv->alloc_id == 0 ||
                    priv->alloc_id > AEROGPU_WDDM_ALLOC_ID_UMD_MAX) {
                    return STATUS_INVALID_PARAMETER;
                }

                const BOOLEAN privShared = (priv->flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED) ? TRUE : FALSE;
                if (privShared != isShared) {
                    return STATUS_INVALID_PARAMETER;
                }
                if (privShared && priv->share_token == 0) {
                    return STATUS_INVALID_PARAMETER;
                }
                if (!privShared && priv->share_token != 0) {
                    return STATUS_INVALID_PARAMETER;
                }
                /*
                 * UMDs may not know the exact allocation size after runtime/KMD
                 * alignment. Accept any non-zero value <= the actual WDDM size.
                 */
                if (priv->size_bytes == 0 || priv->size_bytes > (aerogpu_wddm_u64)info->Size) {
                    return STATUS_INVALID_PARAMETER;
                }

                allocId = (ULONG)priv->alloc_id;
                shareToken = (ULONGLONG)priv->share_token;
            }
        }

        if (allocId == 0) {
            if (isShared) {
                /* Shared allocations must carry AeroGPU private data so the UMD can recover stable IDs on OpenResource. */
                return STATUS_INVALID_PARAMETER;
            }

            allocId = (ULONG)InterlockedIncrement(&adapter->NextKmdAllocId);
            if (allocId < AEROGPU_WDDM_ALLOC_ID_KMD_MIN) {
                AEROGPU_LOG("CreateAllocation: allocation id overflow (wrapped into UMD range), failing with 0x%08lx",
                            STATUS_INTEGER_OVERFLOW);
                /* Roll back allocations already created in this call. */
                for (UINT j = 0; j < i; ++j) {
                    HANDLE hAllocation = pCreate->pAllocationInfo[j].hAllocation;
                    if (hAllocation) {
                        AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
                        pCreate->pAllocationInfo[j].hAllocation = NULL;
                    }
                }
                return STATUS_INTEGER_OVERFLOW;
            }
            shareToken = 0;
        }

        AEROGPU_ALLOCATION* alloc =
            (AEROGPU_ALLOCATION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*alloc), AEROGPU_POOL_TAG);
        if (!alloc) {
            /* Roll back allocations already created in this call. */
            for (UINT j = 0; j < i; ++j) {
                HANDLE hAllocation = pCreate->pAllocationInfo[j].hAllocation;
                if (hAllocation) {
                    AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
                    pCreate->pAllocationInfo[j].hAllocation = NULL;
                }
            }
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        alloc->AllocationId = allocId;
        alloc->ShareToken = shareToken;
        alloc->SizeBytes = info->Size;
        alloc->Flags = (isShared ? AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED : 0);
        alloc->LastKnownPa.QuadPart = 0;

        info->hAllocation = (HANDLE)alloc;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        info->SupportedReadSegmentSet = 1;
        info->SupportedWriteSegmentSet = 1;

        AeroGpuTrackAllocation(adapter, alloc);

#if DBG
        if (logCall) {
            AEROGPU_LOG("CreateAllocation: alloc_id=%lu shared=%lu share_token=0x%I64x size=%Iu",
                        alloc->AllocationId,
                        isShared ? 1ul : 0ul,
                        alloc->ShareToken,
                        alloc->SizeBytes);
        }
#endif
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDestroyAllocation(_In_ const HANDLE hAdapter,
                                                    _In_ const DXGKARG_DESTROYALLOCATION* pDestroy)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!pDestroy) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    for (UINT i = 0; i < pDestroy->NumAllocations; ++i) {
        HANDLE hAllocation = pDestroy->pAllocationList[i].hAllocation;
        AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
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

    /*
     * MVP restriction: shared resources must be single-allocation.
     *
     * Even though the create path rejects multi-allocation shared resources, be
     * defensive here as well: older guests (or future driver changes) may try to
     * open a shared resource that spans multiple allocations (mips/planes/etc).
     * The current shared-surface protocol associates one share token with a
     * single backing allocation, so fail deterministically instead of creating a
     * partially-represented resource.
     */
    if (pOpen->NumAllocations != 1) {
#if DBG
        AEROGPU_LOG("OpenAllocation: rejecting shared resource with NumAllocations=%u (MVP supports only single-allocation shared surfaces)",
                    (unsigned)pOpen->NumAllocations);
#endif
        return STATUS_NOT_SUPPORTED;
    }

    NTSTATUS st = STATUS_SUCCESS;

    for (UINT i = 0; i < pOpen->NumAllocations; ++i) {
        DXGK_OPENALLOCATIONINFO* info = &pOpen->pOpenAllocation[i];

        if (!info->pPrivateDriverData || info->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_private_data)) {
            AEROGPU_LOG("OpenAllocation: missing/too small private data (have=%lu need=%Iu)",
                       (ULONG)info->PrivateDriverDataSize,
                       sizeof(aerogpu_wddm_alloc_private_data));
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        const aerogpu_wddm_alloc_private_data* priv = (const aerogpu_wddm_alloc_private_data*)info->pPrivateDriverData;
        if (priv->magic != AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC ||
            priv->version != AEROGPU_WDDM_ALLOC_PRIVATE_DATA_VERSION || priv->alloc_id == 0 ||
            priv->alloc_id > AEROGPU_WDDM_ALLOC_ID_UMD_MAX) {
            AEROGPU_LOG("OpenAllocation: invalid private data (magic=0x%08lx version=%lu alloc_id=%lu)",
                       (ULONG)priv->magic,
                       (ULONG)priv->version,
                       (ULONG)priv->alloc_id);
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        if ((priv->flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED) == 0 || priv->share_token == 0) {
            AEROGPU_LOG("OpenAllocation: expected shared private data (alloc_id=%lu flags=0x%08lx share_token=0x%I64x)",
                       (ULONG)priv->alloc_id,
                       (ULONG)priv->flags,
                       (ULONGLONG)priv->share_token);
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        if (priv->size_bytes == 0 || priv->size_bytes > (aerogpu_wddm_u64)(SIZE_T)(~(SIZE_T)0)) {
            AEROGPU_LOG("OpenAllocation: invalid size_bytes (alloc_id=%lu size_bytes=%I64u)",
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

        AeroGpuTrackAllocation(adapter, alloc);

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
            AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
            pOpen->pOpenAllocation[j].hAllocation = NULL;
        }
    }
    return st;
}

static NTSTATUS APIENTRY AeroGpuDdiCloseAllocation(_In_ const HANDLE hAdapter,
                                                   _In_ const DXGKARG_CLOSEALLOCATION* pClose)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!pClose) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    for (UINT i = 0; i < pClose->NumAllocations; ++i) {
        HANDLE hAllocation = pClose->pAllocationList[i].hAllocation;
        AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
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

    if (!AllocationCount || !AllocationList) {
        return STATUS_SUCCESS;
    }

    SIZE_T metaSize = FIELD_OFFSET(AEROGPU_SUBMISSION_META, Allocations) +
                      ((SIZE_T)AllocationCount * sizeof(aerogpu_legacy_submission_desc_allocation));

    AEROGPU_SUBMISSION_META* meta =
        (AEROGPU_SUBMISSION_META*)ExAllocatePoolWithTag(NonPagedPool, metaSize, AEROGPU_POOL_TAG);
    if (!meta) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(meta, metaSize);

    meta->AllocationCount = AllocationCount;

    NTSTATUS st =
        AeroGpuBuildAllocTable(AllocationList, AllocationCount, &meta->AllocTableVa, &meta->AllocTablePa, &meta->AllocTableSizeBytes);
    if (!NT_SUCCESS(st)) {
        ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
        return st;
    }

    for (UINT i = 0; i < AllocationCount; ++i) {
        AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)AllocationList[i].hAllocation;
        meta->Allocations[i].allocation_handle = (uint64_t)(ULONG_PTR)AllocationList[i].hAllocation;
        meta->Allocations[i].gpa = (uint64_t)AllocationList[i].PhysicalAddress.QuadPart;
        meta->Allocations[i].size_bytes = (uint32_t)(alloc ? alloc->SizeBytes : 0);
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
    AEROGPU_CONTEXT* ctx = (AEROGPU_CONTEXT*)hContext;
    AEROGPU_ADAPTER* adapter = (ctx && ctx->Device) ? ctx->Device->Adapter : NULL;
    if (!adapter || !pRender || !pRender->pDmaBufferPrivateData) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pRender->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_RENDER;
    priv->Reserved0 = 0;
    priv->MetaHandle = 0;

    if (pRender->AllocationListSize && pRender->pAllocationList) {
        AEROGPU_SUBMISSION_META* meta = NULL;
        NTSTATUS st = AeroGpuBuildAndAttachMeta(pRender->AllocationListSize, pRender->pAllocationList, &meta);
        if (!NT_SUCCESS(st)) {
            return st;
        }

        st = AeroGpuMetaHandleStore(adapter, meta, &priv->MetaHandle);
        if (!NT_SUCCESS(st)) {
            AeroGpuFreeSubmissionMeta(meta);
            return st;
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiPresent(_In_ const HANDLE hContext, _Inout_ DXGKARG_PRESENT* pPresent)
{
    AEROGPU_CONTEXT* ctx = (AEROGPU_CONTEXT*)hContext;
    AEROGPU_ADAPTER* adapter = (ctx && ctx->Device) ? ctx->Device->Adapter : NULL;
    if (!adapter || !pPresent || !pPresent->pDmaBufferPrivateData) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pPresent->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_PRESENT;
    priv->Reserved0 = 0;
    priv->MetaHandle = 0;

    if (pPresent->AllocationListSize && pPresent->pAllocationList) {
        AEROGPU_SUBMISSION_META* meta = NULL;
        NTSTATUS st = AeroGpuBuildAndAttachMeta(pPresent->AllocationListSize, pPresent->pAllocationList, &meta);
        if (!NT_SUCCESS(st)) {
            return st;
        }

        st = AeroGpuMetaHandleStore(adapter, meta, &priv->MetaHandle);
        if (!NT_SUCCESS(st)) {
            AeroGpuFreeSubmissionMeta(meta);
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
    priv->MetaHandle = 0;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSubmitCommand(_In_ const HANDLE hAdapter,
                                                 _In_ const DXGKARG_SUBMITCOMMAND* pSubmitCommand)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pSubmitCommand) {
        return STATUS_INVALID_PARAMETER;
    }

    const ULONGLONG fence = (ULONGLONG)pSubmitCommand->SubmissionFenceId;

    ULONG type = AEROGPU_SUBMIT_PAGING;
    AEROGPU_SUBMISSION_META* meta = NULL;
    if (pSubmitCommand->pDmaBufferPrivateData) {
        const AEROGPU_DMA_PRIV* priv = (const AEROGPU_DMA_PRIV*)pSubmitCommand->pDmaBufferPrivateData;
        type = priv->Type;
        meta = AeroGpuMetaHandleTake(adapter, priv->MetaHandle);
        if (priv->MetaHandle != 0 && !meta) {
            return STATUS_INVALID_PARAMETER;
        }
    }

    PHYSICAL_ADDRESS dmaPa;
    dmaPa.QuadPart = 0;
    PVOID dmaVa = NULL;
    if (pSubmitCommand->DmaBufferSize != 0) {
        dmaVa = AeroGpuAllocContiguous(pSubmitCommand->DmaBufferSize, &dmaPa);
        if (!dmaVa) {
            AeroGpuFreeSubmissionMeta(meta);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlCopyMemory(dmaVa, pSubmitCommand->pDmaBuffer, pSubmitCommand->DmaBufferSize);
    }

    PVOID allocTableVa = NULL;
    PHYSICAL_ADDRESS allocTablePa;
    UINT allocTableSizeBytes = 0;
    UINT allocCount = 0;
    allocTablePa.QuadPart = 0;
    if (meta) {
        allocTableVa = meta->AllocTableVa;
        allocTablePa = meta->AllocTablePa;
        allocTableSizeBytes = meta->AllocTableSizeBytes;
        allocCount = meta->AllocationCount;
    }

    PVOID descVa = NULL;
    SIZE_T descSize = 0;
    PHYSICAL_ADDRESS descPa;
    descPa.QuadPart = 0;

    if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
        descSize = sizeof(aerogpu_legacy_submission_desc_header) +
                   (SIZE_T)allocCount * sizeof(aerogpu_legacy_submission_desc_allocation);

        aerogpu_legacy_submission_desc_header* desc =
            (aerogpu_legacy_submission_desc_header*)AeroGpuAllocContiguous(descSize, &descPa);
        descVa = desc;
        if (!desc) {
            AeroGpuFreeContiguous(dmaVa);
            AeroGpuFreeSubmissionMeta(meta);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        desc->version = AEROGPU_LEGACY_SUBMISSION_DESC_VERSION;
        desc->type = type;
        desc->fence = (uint32_t)fence;
        desc->reserved0 = 0;
        desc->dma_buffer_gpa = (uint64_t)dmaPa.QuadPart;
        desc->dma_buffer_size = pSubmitCommand->DmaBufferSize;
        desc->allocation_count = allocCount;

        if (allocCount && meta) {
            aerogpu_legacy_submission_desc_allocation* out = (aerogpu_legacy_submission_desc_allocation*)(desc + 1);
            RtlCopyMemory(out, meta->Allocations, (SIZE_T)allocCount * sizeof(*out));
        }
    }

    AEROGPU_SUBMISSION* sub =
        (AEROGPU_SUBMISSION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*sub), AEROGPU_POOL_TAG);
    if (!sub) {
        AeroGpuFreeContiguous(descVa);
        AeroGpuFreeContiguous(dmaVa);
        AeroGpuFreeSubmissionMeta(meta);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(sub, sizeof(*sub));
    sub->Fence = fence;
    sub->DmaCopyVa = dmaVa;
    sub->DmaCopySize = pSubmitCommand->DmaBufferSize;
    sub->DmaCopyPa = dmaPa;
    sub->DescVa = descVa;
    sub->DescSize = descSize;
    sub->DescPa = descPa;
    sub->AllocTableVa = NULL;
    sub->AllocTablePa.QuadPart = 0;
    sub->AllocTableSizeBytes = 0;

    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->PendingLock, &oldIrql);

    /*
     * Submit first, then record tracking information, but keep the pending lock
     * held across both so the fence completion DPC can't run before the
     * submission is visible in PendingSubmissions.
     */
    NTSTATUS ringSt = STATUS_SUCCESS;
    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        uint32_t submitFlags = 0;
        if (type == AEROGPU_SUBMIT_PRESENT) {
            submitFlags |= AEROGPU_SUBMIT_FLAG_PRESENT;
        }

        const uint64_t allocTableGpa = allocTableSizeBytes ? (uint64_t)allocTablePa.QuadPart : 0;
        ringSt = AeroGpuV1RingPushSubmit(adapter,
                                         submitFlags,
                                         dmaPa,
                                         pSubmitCommand->DmaBufferSize,
                                         allocTableGpa,
                                         (uint32_t)allocTableSizeBytes,
                                         fence);
    } else {
        ringSt = AeroGpuLegacyRingPushSubmit(adapter, (ULONG)fence, (ULONG)descSize, descPa);
    }

    if (NT_SUCCESS(ringSt)) {
        sub->AllocTableVa = allocTableVa;
        sub->AllocTablePa = allocTablePa;
        sub->AllocTableSizeBytes = allocTableSizeBytes;

        InsertTailList(&adapter->PendingSubmissions, &sub->ListEntry);
        adapter->LastSubmittedFence = fence;
    }

    KeReleaseSpinLock(&adapter->PendingLock, oldIrql);

    if (!NT_SUCCESS(ringSt)) {
        ExFreePoolWithTag(sub, AEROGPU_POOL_TAG);
        AeroGpuFreeContiguous(descVa);
        AeroGpuFreeContiguous(dmaVa);
        AeroGpuFreeSubmissionMeta(meta);
        return ringSt;
    }

    if (meta) {
        ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
    }

    AeroGpuLogSubmission(adapter, (ULONG)fence, type, pSubmitCommand->DmaBufferSize);

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

    BOOLEAN any = FALSE;
    BOOLEAN queueDpc = FALSE;

    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        const ULONG status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
        const ULONG handled = status & (AEROGPU_IRQ_FENCE | AEROGPU_IRQ_SCANOUT_VBLANK | AEROGPU_IRQ_ERROR);
        if (handled == 0) {
            return FALSE;
        }

        /* Ack in the ISR to deassert the (level-triggered) interrupt line. */
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, handled);

        if ((handled & AEROGPU_IRQ_ERROR) != 0) {
            DbgPrintEx(DPFLTR_IHVVIDEO_ID,
                       DPFLTR_ERROR_LEVEL,
                       "aerogpu-kmd: device IRQ error (IRQ_STATUS=0x%08lx)\n",
                       status);
            any = TRUE;
            queueDpc = TRUE;
        }

        if ((handled & AEROGPU_IRQ_FENCE) != 0) {
            const ULONGLONG completedFence64 = adapter->FencePageVa
                                                   ? adapter->FencePageVa->completed_fence
                                                   : ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_COMPLETED_FENCE_LO) |
                                                      ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)
                                                          << 32));

            /*
             * Win7 fences are ULONGs. Clamp to avoid sending a fence that appears
             * to go backwards (e.g. if MMIO tears or the device reports a bogus
             * value).
             */
            ULONG completedFence32 = (ULONG)completedFence64;
            const ULONG lastCompleted32 = (ULONG)adapter->LastCompletedFence;
            const ULONG lastSubmitted32 = (ULONG)adapter->LastSubmittedFence;
            if (completedFence32 < lastCompleted32) {
                completedFence32 = lastCompleted32;
            }
            if (completedFence32 > lastSubmitted32) {
                completedFence32 = lastSubmitted32;
            }

            adapter->LastCompletedFence = (ULONGLONG)completedFence32;
            any = TRUE;
            queueDpc = TRUE;

            if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                DXGKARGCB_NOTIFY_INTERRUPT notify;
                RtlZeroMemory(&notify, sizeof(notify));
                notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
                notify.DmaCompleted.SubmissionFenceId = completedFence32;
                notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
                notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
                adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
            }
        }

        if ((handled & AEROGPU_IRQ_SCANOUT_VBLANK) != 0) {
            /*
             * Keep a guest-time anchor of the most recent vblank so GetScanLine callers don't
             * need to poll the vblank sequence counter at high frequency.
             */
            const ULONGLONG now100ns = KeQueryInterruptTime();
            const ULONGLONG seq = AeroGpuReadRegU64HiLoHi(adapter,
                                                         AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                         AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
            const ULONG periodNs = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
            if (periodNs != 0) {
                adapter->VblankPeriodNs = periodNs;
            }
            AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
            AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, now100ns);

            any = TRUE;
            queueDpc = TRUE;

            if (adapter->DxgkInterface.DxgkCbNotifyInterrupt && adapter->VblankInterruptTypeValid) {
                DXGKARGCB_NOTIFY_INTERRUPT notify;
                RtlZeroMemory(&notify, sizeof(notify));
                notify.InterruptType = adapter->VblankInterruptType;

                /*
                 * DXGKARGCB_NOTIFY_INTERRUPT uses an anonymous union. For vblank-style
                 * interrupts, the payload begins with VidPnSourceId at offset 0 of that
                 * union. Use the known union offset (DmaCompleted) and fill in source 0.
                 */
                *(ULONG*)((PUCHAR)&notify + FIELD_OFFSET(DXGKARGCB_NOTIFY_INTERRUPT, DmaCompleted)) =
                    AEROGPU_VIDPN_SOURCE_ID;
                adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
            }
        }
    } else {
        /* Legacy ABI: only fence interrupts are supported. */
        const ULONG status = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_INT_STATUS);
        if ((status & AEROGPU_LEGACY_INT_FENCE) == 0) {
            return FALSE;
        }

        const ULONGLONG completedFence64 = (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_FENCE_COMPLETED);
        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, AEROGPU_LEGACY_INT_FENCE);

        ULONG completedFence32 = (ULONG)completedFence64;
        const ULONG lastCompleted32 = (ULONG)adapter->LastCompletedFence;
        const ULONG lastSubmitted32 = (ULONG)adapter->LastSubmittedFence;
        if (completedFence32 < lastCompleted32) {
            completedFence32 = lastCompleted32;
        }
        if (completedFence32 > lastSubmitted32) {
            completedFence32 = lastSubmitted32;
        }

        adapter->LastCompletedFence = (ULONGLONG)completedFence32;
        any = TRUE;
        queueDpc = TRUE;

        if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
            DXGKARGCB_NOTIFY_INTERRUPT notify;
            RtlZeroMemory(&notify, sizeof(notify));
            notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
            notify.DmaCompleted.SubmissionFenceId = completedFence32;
            notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
            notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
            adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
        }
    }

    if (queueDpc && adapter->DxgkInterface.DxgkCbQueueDpcForIsr) {
        adapter->DxgkInterface.DxgkCbQueueDpcForIsr(adapter->StartInfo.hDxgkHandle);
    }

    return any;
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

static NTSTATUS APIENTRY AeroGpuDdiControlInterrupt(_In_ const HANDLE hAdapter,
                                                    _In_ const DXGK_INTERRUPT_TYPE InterruptType,
                                                    _In_ BOOLEAN EnableInterrupt)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !adapter->Bar0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * The legacy ABI does not expose an IRQ enable mask and does not deliver
     * scanout vblank interrupts. Treat interrupt control requests as best-effort.
     */
    if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
        return (InterruptType == DXGK_INTERRUPT_TYPE_DMA_COMPLETED) ? STATUS_SUCCESS : STATUS_NOT_SUPPORTED;
    }

    /* Fence/DMA completion interrupt gating. */
    if (InterruptType == DXGK_INTERRUPT_TYPE_DMA_COMPLETED) {
        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
            ULONG enable = adapter->IrqEnableMask;
            if (EnableInterrupt) {
                enable |= AEROGPU_IRQ_FENCE;
            } else {
                enable &= ~AEROGPU_IRQ_FENCE;
            }
            adapter->IrqEnableMask = enable;
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
            if (!EnableInterrupt) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_FENCE);
            }
            KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
        }
        return STATUS_SUCCESS;
    }

    /*
     * Treat all other interrupt types as "vblank/vsync". On Win7, dxgkrnl uses
     * this mechanism to enable/disable vblank delivery for
     * D3DKMTWaitForVerticalBlankEvent and DWM pacing.
     */
    if (!adapter->SupportsVblank) {
        return STATUS_NOT_SUPPORTED;
    }

    adapter->VblankInterruptType = InterruptType;
    adapter->VblankInterruptTypeValid = TRUE;
    KeMemoryBarrier();

    {
        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
        ULONG enable = adapter->IrqEnableMask;
        if (EnableInterrupt) {
            enable |= AEROGPU_IRQ_SCANOUT_VBLANK;
        } else {
            enable &= ~AEROGPU_IRQ_SCANOUT_VBLANK;
        }
        adapter->IrqEnableMask = enable;
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);

        /* Be robust against stale pending bits when disabling. */
        if (!EnableInterrupt) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
        }
        KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
    }

    return STATUS_SUCCESS;
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
    if (adapter->Bar0 && adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        /*
         * Disable IRQs while resetting ring state so we don't race ISR/DPC paths
         * with partially-reset bookkeeping.
         */
        KIRQL irqIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
        KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
    }

    /*
     * Detach the pending submission list under PendingLock so we can free it
     * without racing concurrent SubmitCommand calls.
     */
    LIST_ENTRY pendingToFree;
    InitializeListHead(&pendingToFree);

    ULONGLONG completedFence = 0;
    {
        KIRQL pendingIrql;
        KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);

        completedFence = adapter->LastSubmittedFence;
        adapter->LastCompletedFence = completedFence;

        if (adapter->Bar0) {
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                if (adapter->RingHeader) {
                    const ULONG tail = adapter->RingTail;
                    adapter->RingHeader->head = tail;
                    adapter->RingHeader->tail = tail;
                    KeMemoryBarrier();
                }

                AeroGpuWriteRegU32(adapter,
                                   AEROGPU_MMIO_REG_RING_CONTROL,
                                   AEROGPU_RING_CONTROL_ENABLE | AEROGPU_RING_CONTROL_RESET);
            } else {
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
                adapter->RingTail = 0;
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
            }

            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }

        while (!IsListEmpty(&adapter->PendingSubmissions)) {
            InsertTailList(&pendingToFree, RemoveHeadList(&adapter->PendingSubmissions));
        }

        KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
    }

    if (adapter->Bar0 && adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        KIRQL irqIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->IrqEnableMask);
        KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);
    }

    if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
        DXGKARGCB_NOTIFY_INTERRUPT notify;
        RtlZeroMemory(&notify, sizeof(notify));
        notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
        notify.DmaCompleted.SubmissionFenceId = (ULONG)completedFence;
        notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
        notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
        adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
    }

    if (adapter->DxgkInterface.DxgkCbQueueDpcForIsr) {
        adapter->DxgkInterface.DxgkCbQueueDpcForIsr(adapter->StartInfo.hDxgkHandle);
    }

    AeroGpuMetaHandleFreeAll(adapter);
    while (!IsListEmpty(&pendingToFree)) {
        PLIST_ENTRY entry = RemoveHeadList(&pendingToFree);
        AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
        AeroGpuFreeContiguous(sub->AllocTableVa);
        AeroGpuFreeContiguous(sub->DmaCopyVa);
        AeroGpuFreeContiguous(sub->DescVa);
        ExFreePoolWithTag(sub, AEROGPU_POOL_TAG);
    }
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

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_device_v2_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_query_device_v2_out* out = (aerogpu_escape_query_device_v2_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;

        uint32_t magic = 0;
        uint32_t version = 0;
        uint64_t features = 0;
        if (adapter->Bar0) {
            magic = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_MAGIC);
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                version = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ABI_VERSION);
                features = (uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                           ((uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);
            } else {
                version = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_VERSION);
                /*
                 * Legacy devices do not guarantee FEATURES_LO/HI exist, but some
                 * bring-up device models expose them to allow incremental migration.
                 * If the values look plausible, report them for debugging.
                 */
                if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_FEATURES_HI + sizeof(ULONG))) {
                    const uint64_t maybeFeatures = (uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                                                   ((uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);
                    const uint64_t knownFeatures =
                        AEROGPU_FEATURE_FENCE_PAGE | AEROGPU_FEATURE_CURSOR | AEROGPU_FEATURE_SCANOUT | AEROGPU_FEATURE_VBLANK;
                    if ((maybeFeatures & ~knownFeatures) == 0) {
                        features = maybeFeatures;
                    }
                }
            }
        }

        out->detected_mmio_magic = magic;
        out->abi_version_u32 = version;
        out->features_lo = features;
        out->features_hi = 0;
        out->reserved0 = 0;
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_DEVICE) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_device_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_query_device_out* out = (aerogpu_escape_query_device_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;
        if (!adapter->Bar0) {
            out->mmio_version = 0;
        } else if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            out->mmio_version = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ABI_VERSION);
        } else {
            out->mmio_version = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_VERSION);
        }
        out->reserved0 = 0;
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_FENCE) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_fence_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        ULONGLONG completedFence = adapter->LastCompletedFence;
        if (adapter->Bar0) {
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                completedFence = AeroGpuReadRegU64HiLoHi(adapter,
                                                        AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
                                                        AEROGPU_MMIO_REG_COMPLETED_FENCE_HI);
            } else {
                completedFence = (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_FENCE_COMPLETED);
            }
        }

        aerogpu_escape_query_fence_out* out = (aerogpu_escape_query_fence_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;
        out->last_submitted_fence = (uint64_t)adapter->LastSubmittedFence;
        out->last_completed_fence = (uint64_t)completedFence;
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
        io->ring_size_bytes = adapter->RingSizeBytes;

        io->desc_capacity = (io->desc_capacity > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS)
                                ? AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS
                                : io->desc_capacity;

        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

        ULONG head = 0;
        ULONG tail = 0;
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1 && adapter->RingHeader) {
            head = adapter->RingHeader->head;
            tail = adapter->RingHeader->tail;
        } else if (adapter->Bar0) {
            head = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
            tail = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL);
        }
        io->head = head;
        io->tail = tail;

        ULONG pending = 0;
        if (adapter->RingEntryCount != 0) {
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                pending = tail - head;
                if (pending > adapter->RingEntryCount) {
                    pending = adapter->RingEntryCount;
                }
            } else if (tail >= head) {
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
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1 && adapter->RingHeader) {
                struct aerogpu_submit_desc* ring =
                    (struct aerogpu_submit_desc*)((PUCHAR)adapter->RingVa + sizeof(struct aerogpu_ring_header));
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG idx = (head + i) & (adapter->RingEntryCount - 1);
                    const struct aerogpu_submit_desc entry = ring[idx];
                    io->desc[i].signal_fence = (uint64_t)entry.signal_fence;
                    io->desc[i].cmd_gpa = (uint64_t)entry.cmd_gpa;
                    io->desc[i].cmd_size_bytes = entry.cmd_size_bytes;
                    io->desc[i].flags = entry.flags;
                }
            } else {
                aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)adapter->RingVa;
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG idx = (head + i) % adapter->RingEntryCount;
                    const aerogpu_legacy_ring_entry entry = ring[idx];
                    if (entry.type != AEROGPU_LEGACY_RING_ENTRY_SUBMIT) {
                        continue;
                    }
                    io->desc[i].signal_fence = (uint64_t)entry.submit.fence;
                    io->desc[i].cmd_gpa = 0;
                    io->desc[i].cmd_size_bytes = 0;
                    io->desc[i].flags = entry.submit.flags;

                    /*
                     * Legacy ring entries point at a submission descriptor.
                     * Translate to canonical-ish cmd_gpa/cmd_size_bytes by
                     * peeking the legacy descriptor header.
                     */
                    {
                        PHYSICAL_ADDRESS descPa;
                        descPa.QuadPart = (LONGLONG)entry.submit.desc_gpa;
                        const aerogpu_legacy_submission_desc_header* desc =
                            (const aerogpu_legacy_submission_desc_header*)MmGetVirtualForPhysical(descPa);
                        if (desc) {
                            io->desc[i].signal_fence = (uint64_t)desc->fence;
                            io->desc[i].cmd_gpa = (uint64_t)desc->dma_buffer_gpa;
                            io->desc[i].cmd_size_bytes = desc->dma_buffer_size;
                        } else {
                            /* Fallback: expose the descriptor pointer itself. */
                            io->desc[i].cmd_gpa = (uint64_t)entry.submit.desc_gpa;
                            io->desc[i].cmd_size_bytes = entry.submit.desc_size;
                        }
                    }
                }
            }
        }

        KeReleaseSpinLock(&adapter->RingLock, oldIrql);
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_DUMP_RING_V2) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_dump_ring_v2_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_dump_ring_v2_inout* io = (aerogpu_escape_dump_ring_v2_inout*)pEscape->pPrivateDriverData;

        /* Only ring 0 is currently implemented. */
        if (io->ring_id != 0) {
            return STATUS_NOT_SUPPORTED;
        }

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;
        io->ring_size_bytes = adapter->RingSizeBytes;
        io->reserved0 = 0;
        io->reserved1 = 0;

        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            io->ring_format = AEROGPU_DBGCTL_RING_FORMAT_AGPU;
        } else if (adapter->AbiKind == AEROGPU_ABI_KIND_LEGACY) {
            io->ring_format = AEROGPU_DBGCTL_RING_FORMAT_LEGACY;
        } else {
            io->ring_format = AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN;
        }

        io->desc_capacity = (io->desc_capacity > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS)
                                ? AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS
                                : io->desc_capacity;

        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

        ULONG head = 0;
        ULONG tail = 0;
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1 && adapter->RingHeader) {
            head = adapter->RingHeader->head;
            tail = adapter->RingHeader->tail;
        } else if (adapter->Bar0) {
            head = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
            tail = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL);
        }
        io->head = head;
        io->tail = tail;

        ULONG pending = 0;
        if (adapter->RingEntryCount != 0) {
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                pending = tail - head;
                if (pending > adapter->RingEntryCount) {
                    pending = adapter->RingEntryCount;
                }
            } else if (tail >= head) {
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
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1 && adapter->RingHeader) {
                struct aerogpu_submit_desc* ring =
                    (struct aerogpu_submit_desc*)((PUCHAR)adapter->RingVa + sizeof(struct aerogpu_ring_header));
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG idx = (head + i) & (adapter->RingEntryCount - 1);
                    const struct aerogpu_submit_desc entry = ring[idx];
                    io->desc[i].fence = (uint64_t)entry.signal_fence;
                    io->desc[i].cmd_gpa = (uint64_t)entry.cmd_gpa;
                    io->desc[i].cmd_size_bytes = entry.cmd_size_bytes;
                    io->desc[i].flags = entry.flags;
                    io->desc[i].alloc_table_gpa = (uint64_t)entry.alloc_table_gpa;
                    io->desc[i].alloc_table_size_bytes = entry.alloc_table_size_bytes;
                    io->desc[i].reserved0 = 0;
                }
            } else {
                aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)adapter->RingVa;
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG idx = (head + i) % adapter->RingEntryCount;
                    const aerogpu_legacy_ring_entry entry = ring[idx];
                    if (entry.type != AEROGPU_LEGACY_RING_ENTRY_SUBMIT) {
                        continue;
                    }
                    io->desc[i].fence = (uint64_t)entry.submit.fence;
                    io->desc[i].cmd_gpa = 0;
                    io->desc[i].cmd_size_bytes = 0;
                    io->desc[i].flags = entry.submit.flags;
                    io->desc[i].alloc_table_gpa = 0;
                    io->desc[i].alloc_table_size_bytes = 0;
                    io->desc[i].reserved0 = 0;

                    /*
                     * Legacy ring entries point at a submission descriptor.
                     * Translate to canonical-ish cmd_gpa/cmd_size_bytes by
                     * peeking the legacy descriptor header.
                     */
                    {
                        PHYSICAL_ADDRESS descPa;
                        descPa.QuadPart = (LONGLONG)entry.submit.desc_gpa;
                        const aerogpu_legacy_submission_desc_header* desc =
                            (const aerogpu_legacy_submission_desc_header*)MmGetVirtualForPhysical(descPa);
                        if (desc) {
                            io->desc[i].fence = (uint64_t)desc->fence;
                            io->desc[i].cmd_gpa = (uint64_t)desc->dma_buffer_gpa;
                            io->desc[i].cmd_size_bytes = desc->dma_buffer_size;
                        } else {
                            /* Fallback: expose the descriptor pointer itself. */
                            io->desc[i].cmd_gpa = (uint64_t)entry.submit.desc_gpa;
                            io->desc[i].cmd_size_bytes = entry.submit.desc_size;
                        }
                    }
                }
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

        if (!adapter->Bar0 || !adapter->RingVa || adapter->RingEntryCount == 0 ||
            (adapter->AbiKind == AEROGPU_ABI_KIND_V1 && !adapter->RingHeader)) {
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
        ULONGLONG completedFence = adapter->LastCompletedFence;
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            completedFence = adapter->FencePageVa
                                 ? adapter->FencePageVa->completed_fence
                                 : ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_COMPLETED_FENCE_LO) |
                                    ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_COMPLETED_FENCE_HI) << 32));
        } else {
            completedFence = (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_FENCE_COMPLETED);
        }
        const ULONGLONG fenceNoop = completedFence;

        /*
         * For the new (AGPU) device ABI, command buffers must begin with an
         * `aerogpu_cmd_stream_header`. Use a minimal NOP stream for selftest.
         *
         * For legacy devices, cmd_gpa/cmd_size_bytes is derived from the legacy
         * submission descriptor.
         */
        PVOID dmaVa = NULL;
        PHYSICAL_ADDRESS dmaPa;
        ULONG dmaSizeBytes = 0;
        dmaPa.QuadPart = 0;

        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            dmaSizeBytes = sizeof(struct aerogpu_cmd_stream_header) + sizeof(struct aerogpu_cmd_hdr);
            dmaVa = AeroGpuAllocContiguous(dmaSizeBytes, &dmaPa);
            if (!dmaVa) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES;
                return STATUS_SUCCESS;
            }

            struct aerogpu_cmd_stream_header stream;
            RtlZeroMemory(&stream, sizeof(stream));
            stream.magic = AEROGPU_CMD_STREAM_MAGIC;
            stream.abi_version = AEROGPU_ABI_VERSION_U32;
            stream.size_bytes = (uint32_t)dmaSizeBytes;
            stream.flags = AEROGPU_CMD_STREAM_FLAG_NONE;
            stream.reserved0 = 0;
            stream.reserved1 = 0;

            struct aerogpu_cmd_hdr nop;
            RtlZeroMemory(&nop, sizeof(nop));
            nop.opcode = AEROGPU_CMD_NOP;
            nop.size_bytes = (uint32_t)sizeof(struct aerogpu_cmd_hdr);

            RtlCopyMemory(dmaVa, &stream, sizeof(stream));
            RtlCopyMemory((PUCHAR)dmaVa + sizeof(stream), &nop, sizeof(nop));
        }

        PVOID descVa = NULL;
        PHYSICAL_ADDRESS descPa;
        descPa.QuadPart = 0;

        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
            aerogpu_legacy_submission_desc_header* desc =
                (aerogpu_legacy_submission_desc_header*)AeroGpuAllocContiguous(sizeof(*desc), &descPa);
            descVa = desc;
            if (!desc) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES;
                return STATUS_SUCCESS;
            }

            desc->version = AEROGPU_LEGACY_SUBMISSION_DESC_VERSION;
            desc->type = AEROGPU_SUBMIT_RENDER;
            desc->fence = (uint32_t)fenceNoop;
            desc->reserved0 = 0;
            desc->dma_buffer_gpa = 0;
            desc->dma_buffer_size = 0;
            desc->allocation_count = 0;
        }

        /* Push directly to the ring under RingLock for determinism. */
        ULONG headBefore = 0;
        NTSTATUS pushStatus = STATUS_SUCCESS;
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

        if (NT_SUCCESS(pushStatus)) {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                ULONG head = adapter->RingHeader->head;
                ULONG tail = adapter->RingTail;
                headBefore = head;

                if (NT_SUCCESS(pushStatus) && head != tail) {
                    pushStatus = STATUS_DEVICE_BUSY;
                }

                ULONG pending = tail - head;
                if (NT_SUCCESS(pushStatus) && pending >= adapter->RingEntryCount) {
                    pushStatus = STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
                } else if (NT_SUCCESS(pushStatus)) {
                    const ULONG slot = tail & (adapter->RingEntryCount - 1);
                    struct aerogpu_submit_desc* entry =
                        (struct aerogpu_submit_desc*)((PUCHAR)adapter->RingVa + sizeof(struct aerogpu_ring_header) +
                                                      ((SIZE_T)slot * sizeof(struct aerogpu_submit_desc)));

                    RtlZeroMemory(entry, sizeof(*entry));
                    entry->desc_size_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
                    entry->flags = AEROGPU_SUBMIT_FLAG_NO_IRQ;
                    entry->context_id = 0;
                    entry->engine_id = AEROGPU_ENGINE_0;
                    entry->cmd_gpa = (uint64_t)dmaPa.QuadPart;
                    entry->cmd_size_bytes = dmaSizeBytes;
                    entry->alloc_table_gpa = 0;
                    entry->alloc_table_size_bytes = 0;
                    entry->signal_fence = (uint64_t)fenceNoop;

                    KeMemoryBarrier();
                    adapter->RingTail = tail + 1;
                    adapter->RingHeader->tail = adapter->RingTail;
                    KeMemoryBarrier();

                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_DOORBELL, 1);
                }
            } else {
                ULONG head = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
                ULONG tail = adapter->RingTail;
                headBefore = head;

                if (NT_SUCCESS(pushStatus) && head != tail) {
                    pushStatus = STATUS_DEVICE_BUSY;
                }

                ULONG nextTail = (adapter->RingTail + 1) % adapter->RingEntryCount;
                if (NT_SUCCESS(pushStatus) && nextTail == head) {
                    pushStatus = STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
                } else if (NT_SUCCESS(pushStatus)) {
                    aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)adapter->RingVa;
                    ring[adapter->RingTail].submit.type = AEROGPU_LEGACY_RING_ENTRY_SUBMIT;
                    ring[adapter->RingTail].submit.flags = 0;
                    ring[adapter->RingTail].submit.fence = (ULONG)fenceNoop;
                    ring[adapter->RingTail].submit.desc_size = (ULONG)sizeof(aerogpu_legacy_submission_desc_header);
                    ring[adapter->RingTail].submit.desc_gpa = (uint64_t)descPa.QuadPart;

                    KeMemoryBarrier();
                    adapter->RingTail = nextTail;
                    AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, adapter->RingTail);
                    AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_DOORBELL, 1);
                }
            }

            KeReleaseSpinLock(&adapter->RingLock, oldIrql);
        }

        if (!NT_SUCCESS(pushStatus)) {
            AeroGpuFreeContiguous(descVa);
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
            ULONG headNow = (adapter->AbiKind == AEROGPU_ABI_KIND_V1 && adapter->RingHeader)
                                ? adapter->RingHeader->head
                                : AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
            if (headNow != headBefore) {
                testStatus = STATUS_SUCCESS;
                break;
            }

            LARGE_INTEGER interval;
            interval.QuadPart = -10000; /* 1ms */
            KeDelayExecutionThread(KernelMode, FALSE, &interval);
        }

        if (NT_SUCCESS(testStatus)) {
            AeroGpuFreeContiguous(descVa);
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

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_VBLANK) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_vblank_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        if (!adapter->Bar0) {
            return STATUS_DEVICE_NOT_READY;
        }

        aerogpu_escape_query_vblank_out* out = (aerogpu_escape_query_vblank_out*)pEscape->pPrivateDriverData;

        /* Only scanout/source 0 is currently implemented; ignore other values. */
        out->vidpn_source_id = AEROGPU_VIDPN_SOURCE_ID;

        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;

        const BOOLEAN haveIrqRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG));
        if (haveIrqRegs) {
            out->irq_enable = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE);
            out->irq_status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
        } else {
            out->irq_enable = 0;
            out->irq_status = 0;
        }

        out->flags = AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID;

        const BOOLEAN haveVblankRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS + sizeof(ULONG));

        const ULONGLONG features = (adapter->Bar0Length >= (AEROGPU_MMIO_REG_FEATURES_HI + sizeof(ULONG)))
                                      ? ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                                         ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32))
                                      : 0;

        BOOLEAN featuresPlausible = TRUE;
        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
            const ULONGLONG knownFeatures = AEROGPU_FEATURE_FENCE_PAGE | AEROGPU_FEATURE_CURSOR | AEROGPU_FEATURE_SCANOUT |
                                            AEROGPU_FEATURE_VBLANK;
            featuresPlausible = (features & ~knownFeatures) == 0;
        }

        const BOOLEAN supported = haveVblankRegs && featuresPlausible && ((features & AEROGPU_FEATURE_VBLANK) != 0);
        if (supported) {
            out->flags |= AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED;
            out->vblank_seq = AeroGpuReadRegU64HiLoHi(adapter,
                                                      AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                      AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
            out->last_vblank_time_ns = AeroGpuReadRegU64HiLoHi(adapter,
                                                               AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
                                                               AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
            out->vblank_period_ns = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
        } else {
            out->vblank_seq = 0;
            out->last_vblank_time_ns = 0;
            out->vblank_period_ns = 0;
        }
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
    init.DxgkDdiControlInterrupt = AeroGpuDdiControlInterrupt;
    init.DxgkDdiGetScanLine = AeroGpuDdiGetScanLine;
    init.DxgkDdiResetFromTimeout = AeroGpuDdiResetFromTimeout;
    init.DxgkDdiRestartFromTimeout = AeroGpuDdiRestartFromTimeout;

    init.DxgkDdiSetPointerPosition = AeroGpuDdiSetPointerPosition;
    init.DxgkDdiSetPointerShape = AeroGpuDdiSetPointerShape;

    init.DxgkDdiEscape = AeroGpuDdiEscape;

    return DxgkInitialize(DriverObject, RegistryPath, &init);
}
