#pragma once

#include <ntddk.h>
#include <storport.h>

#ifndef AEROVIRTIO_ALIGN_UP
#define AEROVIRTIO_ALIGN_UP(value, alignment) (((value) + ((alignment)-1)) & ~((alignment)-1))
#endif

#define AEROVIRTQ_DESC_F_NEXT 0x0001
#define AEROVIRTQ_DESC_F_WRITE 0x0002
#define AEROVIRTQ_DESC_F_INDIRECT 0x0004

typedef struct _AEROVIRTQ_DESC {
    ULONGLONG addr;
    ULONG len;
    USHORT flags;
    USHORT next;
} AEROVIRTQ_DESC, *PAEROVIRTQ_DESC;

typedef struct _AEROVIRTQ_AVAIL {
    USHORT flags;
    USHORT idx;
    USHORT ring[1];
} AEROVIRTQ_AVAIL, *PAEROVIRTQ_AVAIL;

typedef struct _AEROVIRTQ_USED_ELEM {
    ULONG id;
    ULONG len;
} AEROVIRTQ_USED_ELEM, *PAEROVIRTQ_USED_ELEM;

typedef struct _AEROVIRTQ_USED {
    USHORT flags;
    USHORT idx;
    AEROVIRTQ_USED_ELEM ring[1];
} AEROVIRTQ_USED, *PAEROVIRTQ_USED;

typedef struct _AEROVIRTQ {
    USHORT QueueIndex;
    USHORT QueueSize;

    PVOID RingVa;
    STOR_PHYSICAL_ADDRESS RingPa;
    ULONG RingBytes;

    volatile AEROVIRTQ_DESC* Desc;
    volatile AEROVIRTQ_AVAIL* Avail;
    volatile AEROVIRTQ_USED* Used;

    USHORT AvailIdxShadow;
    USHORT LastUsedIdx;

    USHORT FreeCount;
    USHORT* FreeStack;
} AEROVIRTQ, *PAEROVIRTQ;

ULONG AerovirtqGetRingBytes(_In_ USHORT queueSize);

BOOLEAN AerovirtqInit(
    _In_ PVOID hwDeviceExtension,
    _Inout_ PAEROVIRTQ vq,
    _In_ USHORT queueIndex,
    _In_ USHORT queueSize,
    _In_ PVOID ringVa,
    _In_ STOR_PHYSICAL_ADDRESS ringPa,
    _In_ ULONG ringBytes);

USHORT AerovirtqAllocDesc(_Inout_ PAEROVIRTQ vq);
VOID AerovirtqFreeDesc(_Inout_ PAEROVIRTQ vq, _In_ USHORT descIndex);
VOID AerovirtqFreeChain(_Inout_ PAEROVIRTQ vq, _In_ USHORT headDescIndex);

VOID AerovirtqSubmit(_Inout_ PAEROVIRTQ vq, _In_ USHORT headDescIndex);

BOOLEAN AerovirtqPopUsed(
    _Inout_ PAEROVIRTQ vq,
    _Out_ USHORT* headDescIndex,
    _Out_opt_ ULONG* usedLen);
