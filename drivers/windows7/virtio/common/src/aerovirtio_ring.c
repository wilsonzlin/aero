#include <ntddk.h>
#include <storport.h>

#include "../include/aerovirtio_ring.h"

ULONG AerovirtqGetRingBytes(_In_ USHORT queueSize)
{
    const ULONG descBytes = sizeof(AEROVIRTQ_DESC) * (ULONG)queueSize;
    const ULONG availBytes = FIELD_OFFSET(AEROVIRTQ_AVAIL, ring) + sizeof(USHORT) * (ULONG)queueSize;
    const ULONG usedBytes = FIELD_OFFSET(AEROVIRTQ_USED, ring) + sizeof(AEROVIRTQ_USED_ELEM) * (ULONG)queueSize;

    const ULONG usedOffset = AEROVIRTIO_ALIGN_UP(descBytes + availBytes, PAGE_SIZE);
    return usedOffset + AEROVIRTIO_ALIGN_UP(usedBytes, PAGE_SIZE);
}

BOOLEAN AerovirtqInit(
    _In_ PVOID hwDeviceExtension,
    _Inout_ PAEROVIRTQ vq,
    _In_ USHORT queueIndex,
    _In_ USHORT queueSize,
    _In_ PVOID ringVa,
    _In_ STOR_PHYSICAL_ADDRESS ringPa,
    _In_ ULONG ringBytes)
{
    if (vq == NULL || ringVa == NULL || queueSize == 0) {
        return FALSE;
    }

    RtlZeroMemory(vq, sizeof(*vq));
    vq->QueueIndex = queueIndex;
    vq->QueueSize = queueSize;

    vq->RingVa = ringVa;
    vq->RingPa = ringPa;
    vq->RingBytes = ringBytes;

    RtlZeroMemory(ringVa, ringBytes);

    vq->Desc = (volatile AEROVIRTQ_DESC*)ringVa;

    const ULONG descBytes = sizeof(AEROVIRTQ_DESC) * (ULONG)queueSize;
    const ULONG availBytes = FIELD_OFFSET(AEROVIRTQ_AVAIL, ring) + sizeof(USHORT) * (ULONG)queueSize;
    const ULONG usedOffset = AEROVIRTIO_ALIGN_UP(descBytes + availBytes, PAGE_SIZE);

    vq->Avail = (volatile AEROVIRTQ_AVAIL*)((PUCHAR)ringVa + descBytes);
    vq->Used = (volatile AEROVIRTQ_USED*)((PUCHAR)ringVa + usedOffset);

    vq->AvailIdxShadow = 0;
    vq->LastUsedIdx = 0;

    vq->FreeStack = (USHORT*)StorPortAllocatePool(hwDeviceExtension, sizeof(USHORT) * (ULONG)queueSize, 'qVrA');
    if (vq->FreeStack == NULL) {
        return FALSE;
    }

    for (USHORT i = 0; i < queueSize; ++i) {
        vq->FreeStack[i] = (USHORT)(queueSize - 1 - i);
    }
    vq->FreeCount = queueSize;

    return TRUE;
}

USHORT AerovirtqAllocDesc(_Inout_ PAEROVIRTQ vq)
{
    if (vq->FreeCount == 0) {
        return 0xFFFF;
    }

    const USHORT idx = vq->FreeStack[vq->FreeCount - 1];
    vq->FreeCount--;
    return idx;
}

VOID AerovirtqFreeDesc(_Inout_ PAEROVIRTQ vq, _In_ USHORT descIndex)
{
    if (vq->FreeCount >= vq->QueueSize) {
        return;
    }

    vq->FreeStack[vq->FreeCount] = descIndex;
    vq->FreeCount++;
}

VOID AerovirtqFreeChain(_Inout_ PAEROVIRTQ vq, _In_ USHORT headDescIndex)
{
    volatile AEROVIRTQ_DESC* desc = vq->Desc;
    USHORT idx = headDescIndex;

    if (desc[idx].flags & AEROVIRTQ_DESC_F_INDIRECT) {
        AerovirtqFreeDesc(vq, idx);
        return;
    }

    for (;;) {
        const USHORT flags = desc[idx].flags;
        const USHORT next = desc[idx].next;
        AerovirtqFreeDesc(vq, idx);
        if ((flags & AEROVIRTQ_DESC_F_NEXT) == 0) {
            break;
        }
        idx = next;
    }
}

VOID AerovirtqSubmit(_Inout_ PAEROVIRTQ vq, _In_ USHORT headDescIndex)
{
    const USHORT availIdx = vq->AvailIdxShadow;
    const USHORT slot = (USHORT)(availIdx % vq->QueueSize);
    vq->Avail->ring[slot] = headDescIndex;

    KeMemoryBarrier();

    vq->AvailIdxShadow = (USHORT)(availIdx + 1);
    vq->Avail->idx = vq->AvailIdxShadow;
}

BOOLEAN AerovirtqPopUsed(
    _Inout_ PAEROVIRTQ vq,
    _Out_ USHORT* headDescIndex,
    _Out_opt_ ULONG* usedLen)
{
    const USHORT usedIdx = vq->Used->idx;

    KeMemoryBarrier();

    if (vq->LastUsedIdx == usedIdx) {
        return FALSE;
    }

    const USHORT slot = (USHORT)(vq->LastUsedIdx % vq->QueueSize);
    const AEROVIRTQ_USED_ELEM elem = vq->Used->ring[slot];

    vq->LastUsedIdx = (USHORT)(vq->LastUsedIdx + 1);

    if (headDescIndex != NULL) {
        *headDescIndex = (USHORT)elem.id;
    }
    if (usedLen != NULL) {
        *usedLen = elem.len;
    }

    return TRUE;
}

