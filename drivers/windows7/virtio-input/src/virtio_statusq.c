#include "virtio_statusq.h"

#include <stddef.h>

/* -------------------------------------------------------------------------- */
/* Portable helpers (host-buildable unit tests)                                */
/* -------------------------------------------------------------------------- */

bool VirtioStatusQCookieToIndex(
    const void* TxBase,
    size_t TxStride,
    uint16_t TxBufferCount,
    const void* Cookie,
    uint16_t* IndexOut)
{
    /*
     * Use integer address arithmetic to remain robust even if the cookie is
     * corrupted. uintptr_t is optional in older MSVC/WDK environments; use
     * ULONG_PTR when available.
     */
#if defined(_WIN32)
    ULONG_PTR base;
    ULONG_PTR p;
#else
    uintptr_t base;
    uintptr_t p;
#endif
    size_t off;
    size_t idx;

    if (TxBase == NULL || Cookie == NULL || IndexOut == NULL || TxStride == 0 || TxBufferCount == 0) {
        return false;
    }

#if defined(_WIN32)
    base = (ULONG_PTR)TxBase;
    p = (ULONG_PTR)Cookie;
#else
    base = (uintptr_t)TxBase;
    p = (uintptr_t)Cookie;
#endif
    if (p < base) {
        return false;
    }

    off = (size_t)(p - base);
    if ((off % TxStride) != 0) {
        return false;
    }

    idx = off / TxStride;
    if (idx >= (size_t)TxBufferCount) {
        return false;
    }

    *IndexOut = (uint16_t)idx;
    return true;
}

void VirtioStatusQCoalesceSimInit(VIOINPUT_STATUSQ_COALESCE_SIM* Sim, uint16_t Capacity, bool DropOnFull)
{
    if (Sim == NULL) {
        return;
    }
    Sim->Capacity = Capacity;
    Sim->FreeCount = Capacity;
    Sim->DropOnFull = DropOnFull;
    Sim->PendingValid = false;
    Sim->PendingLedBitfield = 0;
}

bool VirtioStatusQCoalesceSimWrite(VIOINPUT_STATUSQ_COALESCE_SIM* Sim, uint8_t LedBitfield)
{
    if (Sim == NULL || Sim->Capacity == 0) {
        return false;
    }

    Sim->PendingLedBitfield = LedBitfield;
    Sim->PendingValid = true;

    if (Sim->FreeCount == 0) {
        if (Sim->DropOnFull) {
            Sim->PendingValid = false;
        }
        return false;
    }

    /* Submit immediately. */
    Sim->FreeCount--;
    Sim->PendingValid = false;
    return true;
}

bool VirtioStatusQCoalesceSimComplete(VIOINPUT_STATUSQ_COALESCE_SIM* Sim)
{
    bool submitted;

    if (Sim == NULL || Sim->Capacity == 0) {
        return false;
    }

    /* Return one buffer slot (completion). */
    if (Sim->FreeCount < Sim->Capacity) {
        Sim->FreeCount++;
    }

    submitted = false;
    if (Sim->PendingValid) {
        if (Sim->FreeCount == 0) {
            if (Sim->DropOnFull) {
                Sim->PendingValid = false;
            }
        } else {
            Sim->FreeCount--;
            Sim->PendingValid = false;
            submitted = true;
        }
    }

    return submitted;
}

#ifdef _WIN32

#include "virtqueue_split.h"

#include <ntddk.h>
#include <wdf.h>

#include "virtio_input.h"
#include "led_translate.h"

/*
 * StatusQ buffers are sized in units of VIOINPUT_STATUSQ_EVENTS_PER_BUFFER.
 * Ensure the LED translation helper never produces more events than will fit.
 */
C_ASSERT(VIOINPUT_STATUSQ_EVENTS_PER_BUFFER == LED_TRANSLATE_EVENT_COUNT);

#if defined(_MSC_VER)
#define VIOINPUT_STATUSQ_POOL_TAG 'qSoV'
#else
#define VIOINPUT_STATUSQ_MAKE_POOL_TAG(a, b, c, d) \
    ((ULONG)(((ULONG)(a) << 24) | ((ULONG)(b) << 16) | ((ULONG)(c) << 8) | ((ULONG)(d))))
#define VIOINPUT_STATUSQ_POOL_TAG VIOINPUT_STATUSQ_MAKE_POOL_TAG('q', 'S', 'o', 'V')
#endif

typedef struct _VIRTIO_STATUSQ {
    WDFDEVICE Device;
    PVIRTIO_PCI_DEVICE PciDevice;
    USHORT QueueIndex;

    VIRTQ_SPLIT* Vq;
    WDFCOMMONBUFFER RingCommonBuffer;

    WDFCOMMONBUFFER TxCommonBuffer;
    PVOID TxVa;
    UINT64 TxPa;
    UINT16 TxBufferCount;
    UINT32 TxBufferStride;

    UINT16 FreeHead;
    UINT16 FreeCount;
    UINT16* NextFree;

    WDFSPINLOCK Lock;
    BOOLEAN Active;
    BOOLEAN DropOnFull;

    /*
     * Mask of virtio-input EV_LED codes (0..4) advertised by the device via
     * EV_BITS(EV_LED). Used to filter HID LED output reports so we only emit
     * supported LED events.
     *
     * If this is 0 (unknown), the translation helper falls back to emitting
     * only the required LEDs (NumLock/CapsLock/ScrollLock).
     */
    UCHAR KeyboardLedSupportedMask;

    BOOLEAN PendingValid;
    UCHAR PendingLedBitfield;
} VIRTIO_STATUSQ, *PVIRTIO_STATUSQ;

static __forceinline PUCHAR VirtioStatusQTxBufVa(_In_ const VIRTIO_STATUSQ* Q, _In_ UINT16 Index)
{
    return (PUCHAR)Q->TxVa + ((SIZE_T)Index * (SIZE_T)Q->TxBufferStride);
}

static __forceinline UINT64 VirtioStatusQTxBufPa(_In_ const VIRTIO_STATUSQ* Q, _In_ UINT16 Index)
{
    return Q->TxPa + ((UINT64)Index * (UINT64)Q->TxBufferStride);
}

static __forceinline VOID VirtioStatusQLock(_Inout_ PVIRTIO_STATUSQ Q)
{
    if (Q != NULL && Q->Lock != NULL) {
        WdfSpinLockAcquire(Q->Lock);
    }
}

static __forceinline VOID VirtioStatusQUnlock(_Inout_ PVIRTIO_STATUSQ Q)
{
    if (Q != NULL && Q->Lock != NULL) {
        WdfSpinLockRelease(Q->Lock);
    }
}

static __forceinline VOID VirtioStatusQUpdateDepthCounter(_In_ PVIRTIO_STATUSQ Q)
{
    PDEVICE_CONTEXT devCtx;
    LONG depth;

    devCtx = VirtioInputGetDeviceContext(Q->Device);
    depth = (Q->Vq == NULL) ? 0 : (LONG)(Q->Vq->qsz - Q->Vq->num_free);
    VioInputCounterSet(&devCtx->Counters.VirtioQueueDepth, depth);
    VioInputCounterMaxUpdate(&devCtx->Counters.VirtioQueueMaxDepth, depth);
}

static __forceinline VOID VirtioStatusQCountDrop(_In_ PVIRTIO_STATUSQ Q)
{
    PDEVICE_CONTEXT devCtx;

    if (Q == NULL || Q->Device == NULL) {
        return;
    }

    devCtx = VirtioInputGetDeviceContext(Q->Device);
    if (devCtx == NULL) {
        return;
    }

    VioInputCounterInc(&devCtx->Counters.VirtioStatusDrops);
}

static __forceinline UINT16 VirtioStatusQPopFreeTxBuffer(_Inout_ PVIRTIO_STATUSQ Q)
{
    UINT16 idx;

    if (Q->FreeCount == 0) {
        if (Q->FreeHead != VIRTQ_SPLIT_NO_DESC) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "statusq free list inconsistent: freeCount=0 freeHead=%u\n",
                (ULONG)Q->FreeHead);
            Q->FreeHead = VIRTQ_SPLIT_NO_DESC;
        }
        return VIRTQ_SPLIT_NO_DESC;
    }

    if (Q->FreeHead == VIRTQ_SPLIT_NO_DESC) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "statusq free list inconsistent: freeCount=%u freeHead=NO_DESC\n",
            (ULONG)Q->FreeCount);
        Q->FreeCount = 0;
        return VIRTQ_SPLIT_NO_DESC;
    }

    idx = Q->FreeHead;
    if (idx >= Q->TxBufferCount) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "statusq free list head out of range: head=%u txCount=%u\n",
            (ULONG)idx,
            (ULONG)Q->TxBufferCount);
        Q->FreeHead = VIRTQ_SPLIT_NO_DESC;
        Q->FreeCount = 0;
        return VIRTQ_SPLIT_NO_DESC;
    }

    Q->FreeHead = Q->NextFree[idx];
    if (Q->FreeHead != VIRTQ_SPLIT_NO_DESC && Q->FreeHead >= Q->TxBufferCount) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "statusq free list next out of range: next=%u txCount=%u\n",
            (ULONG)Q->FreeHead,
            (ULONG)Q->TxBufferCount);
        Q->FreeHead = VIRTQ_SPLIT_NO_DESC;
        Q->FreeCount = 0;
        Q->NextFree[idx] = VIRTQ_SPLIT_NO_DESC;
        return VIRTQ_SPLIT_NO_DESC;
    }

    Q->NextFree[idx] = VIRTQ_SPLIT_NO_DESC;
    Q->FreeCount--;
    return idx;
}

static __forceinline VOID VirtioStatusQPushFreeTxBuffer(_Inout_ PVIRTIO_STATUSQ Q, _In_ UINT16 Index)
{
    if (Index >= Q->TxBufferCount) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq free list push invalid index=%u\n", (ULONG)Index);
        return;
    }
    if (Q->FreeCount >= Q->TxBufferCount) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "statusq free list overflow: freeCount=%u txCount=%u\n",
            (ULONG)Q->FreeCount,
            (ULONG)Q->TxBufferCount);
        return;
    }

    Q->NextFree[Index] = Q->FreeHead;
    Q->FreeHead = Index;
    Q->FreeCount++;
}

static NTSTATUS VirtioStatusQTrySubmit(_Inout_ PVIRTIO_STATUSQ Q)
{
    UINT16 idx;
    PUCHAR bufVa;
    UINT64 bufPa;
    size_t eventCount;
    UINT32 bytes;
    VIRTQ_SG sg;
    UINT16 head;
    NTSTATUS status;
    PDEVICE_CONTEXT devCtx;

    if (Q == NULL || Q->PciDevice == NULL || Q->Vq == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    devCtx = (Q->Device != NULL) ? VirtioInputGetDeviceContext(Q->Device) : NULL;

    if (!Q->Active || !Q->PendingValid) {
        return STATUS_SUCCESS;
    }

    idx = VirtioStatusQPopFreeTxBuffer(Q);
    if (idx == VIRTQ_SPLIT_NO_DESC) {
        if (devCtx != NULL) {
            VioInputCounterInc(&devCtx->Counters.StatusQFull);
        }
        if (Q->DropOnFull) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
                "statusq dropping pending LED report (queue full): leds=0x%02X\n",
                (ULONG)Q->PendingLedBitfield);
            VirtioStatusQCountDrop(Q);
            Q->PendingValid = FALSE;
            if (devCtx != NULL) {
                VioInputCounterInc(&devCtx->Counters.LedWritesDropped);
            }
        }
        return STATUS_SUCCESS;
    }

    bufVa = VirtioStatusQTxBufVa(Q, idx);
    bufPa = VirtioStatusQTxBufPa(Q, idx);

    eventCount = led_translate_build_virtio_events(
        (uint8_t)Q->PendingLedBitfield,
        (uint8_t)Q->KeyboardLedSupportedMask,
        (struct virtio_input_event_le*)bufVa);
    if (eventCount == 0) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq led_translate returned 0 events\n");
        if (devCtx != NULL) {
            VioInputCounterInc(&devCtx->Counters.LedWritesDropped);
        }
        VirtioStatusQPushFreeTxBuffer(Q, idx);
        Q->PendingValid = FALSE;
        return STATUS_SUCCESS;
    }
    if (eventCount > VIOINPUT_STATUSQ_EVENTS_PER_BUFFER) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "statusq led_translate returned too many events: count=%Iu cap=%u\n",
            eventCount,
            (ULONG)VIOINPUT_STATUSQ_EVENTS_PER_BUFFER);
        if (devCtx != NULL) {
            VioInputCounterInc(&devCtx->Counters.LedWritesDropped);
        }
        VirtioStatusQPushFreeTxBuffer(Q, idx);
        Q->PendingValid = FALSE;
        return STATUS_SUCCESS;
    }
    bytes = (UINT32)(eventCount * sizeof(struct virtio_input_event_le));

    sg.addr = bufPa;
    sg.len = bytes;
    sg.write = FALSE;

    head = VIRTQ_SPLIT_NO_DESC;
    status = VirtqSplitAddBuffer(Q->Vq, &sg, 1, bufVa, &head);
    if (!NT_SUCCESS(status)) {
        VirtioStatusQPushFreeTxBuffer(Q, idx);
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq VirtqSplitAddBuffer failed: %!STATUS!\n", status);
        if (status == STATUS_INSUFFICIENT_RESOURCES && devCtx != NULL) {
            VioInputCounterInc(&devCtx->Counters.StatusQFull);
        }
        if (Q->DropOnFull) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_VIRTQ,
                "statusq dropping pending LED report (VirtqSplitAddBuffer failed): leds=0x%02X\n",
                (ULONG)Q->PendingLedBitfield);
            VirtioStatusQCountDrop(Q);
            Q->PendingValid = FALSE;
            if (devCtx != NULL) {
                VioInputCounterInc(&devCtx->Counters.LedWritesDropped);
            }
        }
        return STATUS_SUCCESS;
    }

    Q->PendingValid = FALSE;

    VirtqSplitPublish(Q->Vq, head);
    if (VirtqSplitKickPrepare(Q->Vq)) {
        VirtioPciNotifyQueue(Q->PciDevice, Q->QueueIndex);
    }
    VirtqSplitKickCommit(Q->Vq);

    if (devCtx != NULL) {
        VioInputCounterInc(&devCtx->Counters.StatusQSubmits);
        VioInputCounterInc(&devCtx->Counters.LedWritesSubmitted);
    }

    VirtioStatusQUpdateDepthCounter(Q);
    return STATUS_SUCCESS;
}

NTSTATUS
VirtioStatusQInitialize(
    _Out_ PVIRTIO_STATUSQ* StatusQ,
    _In_ WDFDEVICE Device,
    _Inout_ PVIRTIO_PCI_DEVICE PciDevice,
    _In_ WDFDMAENABLER DmaEnabler,
    _In_ USHORT QueueIndex,
    _In_ USHORT QueueSize)
{
    NTSTATUS status;
    PVIRTIO_STATUSQ q;
    size_t vqBytes;
    size_t ringBytes;
    PVOID ringVa;
    PHYSICAL_ADDRESS ringPa;
    WDF_OBJECT_ATTRIBUTES attributes;
    size_t txBytes;
    PHYSICAL_ADDRESS txPa;

    if (StatusQ == NULL || Device == NULL || PciDevice == NULL || DmaEnabler == NULL || QueueSize == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    *StatusQ = NULL;

    q = (PVIRTIO_STATUSQ)ExAllocatePoolWithTag(NonPagedPool, sizeof(*q), VIOINPUT_STATUSQ_POOL_TAG);
    if (q == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(q, sizeof(*q));

    q->Device = Device;
    q->PciDevice = PciDevice;
    q->QueueIndex = QueueIndex;
    q->TxBufferStride = (UINT32)(sizeof(struct virtio_input_event_le) * VIOINPUT_STATUSQ_EVENTS_PER_BUFFER);
    q->TxBufferCount = QueueSize;
    q->DropOnFull = FALSE;

    q->NextFree = (UINT16*)ExAllocatePoolWithTag(NonPagedPool, sizeof(UINT16) * (SIZE_T)q->TxBufferCount, VIOINPUT_STATUSQ_POOL_TAG);
    if (q->NextFree == NULL) {
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    vqBytes = VirtqSplitStateSize(QueueSize);
    q->Vq = (VIRTQ_SPLIT*)ExAllocatePoolWithTag(NonPagedPool, vqBytes, VIOINPUT_STATUSQ_POOL_TAG);
    if (q->Vq == NULL) {
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    ringBytes = VirtqSplitRingMemSize(QueueSize, 4, FALSE);
    if (ringBytes == 0) {
        ExFreePoolWithTag(q->Vq, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return STATUS_INVALID_PARAMETER;
    }

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;

    status = WdfSpinLockCreate(&attributes, &q->Lock);
    if (!NT_SUCCESS(status)) {
        ExFreePoolWithTag(q->Vq, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return status;
    }

    status = WdfCommonBufferCreate(DmaEnabler, ringBytes, &attributes, &q->RingCommonBuffer);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(q->Lock);
        ExFreePoolWithTag(q->Vq, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return status;
    }

    ringVa = WdfCommonBufferGetAlignedVirtualAddress(q->RingCommonBuffer);
    ringPa = WdfCommonBufferGetAlignedLogicalAddress(q->RingCommonBuffer);
    RtlZeroMemory(ringVa, ringBytes);

    status = VirtqSplitInit(q->Vq, QueueSize, FALSE, TRUE, ringVa, (UINT64)ringPa.QuadPart, 4, NULL, 0, 0, 0);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(q->RingCommonBuffer);
        WdfObjectDelete(q->Lock);
        ExFreePoolWithTag(q->Vq, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return status;
    }

    txBytes = (SIZE_T)q->TxBufferStride * (SIZE_T)q->TxBufferCount;
    status = WdfCommonBufferCreate(DmaEnabler, txBytes, &attributes, &q->TxCommonBuffer);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(q->RingCommonBuffer);
        WdfObjectDelete(q->Lock);
        ExFreePoolWithTag(q->Vq, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return status;
    }

    q->TxVa = WdfCommonBufferGetAlignedVirtualAddress(q->TxCommonBuffer);
    txPa = WdfCommonBufferGetAlignedLogicalAddress(q->TxCommonBuffer);
    q->TxPa = (UINT64)txPa.QuadPart;
    RtlZeroMemory(q->TxVa, txBytes);

    VirtioStatusQReset(q);

    *StatusQ = q;
    return STATUS_SUCCESS;
}

VOID
VirtioStatusQUninitialize(_In_ PVIRTIO_STATUSQ StatusQ)
{
    PVIRTIO_STATUSQ q;

    q = StatusQ;
    if (q == NULL) {
        return;
    }

    if (q->Lock != NULL) {
        WdfObjectDelete(q->Lock);
        q->Lock = NULL;
    }

    if (q->TxCommonBuffer != NULL) {
        WdfObjectDelete(q->TxCommonBuffer);
        q->TxCommonBuffer = NULL;
    }

    if (q->RingCommonBuffer != NULL) {
        WdfObjectDelete(q->RingCommonBuffer);
        q->RingCommonBuffer = NULL;
    }

    if (q->Vq != NULL) {
        ExFreePoolWithTag(q->Vq, VIOINPUT_STATUSQ_POOL_TAG);
        q->Vq = NULL;
    }

    if (q->NextFree != NULL) {
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        q->NextFree = NULL;
    }

    ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
}

VOID
VirtioStatusQReset(_In_ PVIRTIO_STATUSQ StatusQ)
{
    PVIRTIO_STATUSQ q;
    UINT16 i;

    q = StatusQ;
    if (q == NULL) {
        return;
    }

    VirtioStatusQLock(q);

    if (q->Vq != NULL) {
        VirtqSplitReset(q->Vq);
    }

    q->PendingValid = FALSE;
    q->PendingLedBitfield = 0;

    q->FreeHead = (q->TxBufferCount == 0) ? VIRTQ_SPLIT_NO_DESC : 0;
    q->FreeCount = q->TxBufferCount;
    for (i = 0; i < q->TxBufferCount; i++) {
        q->NextFree[i] = (i + 1 < q->TxBufferCount) ? (UINT16)(i + 1) : VIRTQ_SPLIT_NO_DESC;
    }

    if (q->Device != NULL) {
        VirtioStatusQUpdateDepthCounter(q);
    }

    VirtioStatusQUnlock(q);
}

VOID
VirtioStatusQGetRingAddresses(_In_ PVIRTIO_STATUSQ StatusQ, _Out_ UINT64* DescPa, _Out_ UINT64* AvailPa, _Out_ UINT64* UsedPa)
{
    PVIRTIO_STATUSQ q;

    if (DescPa != NULL) {
        *DescPa = 0;
    }
    if (AvailPa != NULL) {
        *AvailPa = 0;
    }
    if (UsedPa != NULL) {
        *UsedPa = 0;
    }

    q = StatusQ;
    if (q == NULL) {
        return;
    }

    VirtioStatusQLock(q);
    if (q->Vq == NULL) {
        VirtioStatusQUnlock(q);
        return;
    }

    if (DescPa != NULL) {
        *DescPa = q->Vq->desc_pa;
    }
    if (AvailPa != NULL) {
        *AvailPa = q->Vq->avail_pa;
    }
    if (UsedPa != NULL) {
        *UsedPa = q->Vq->used_pa;
    }

    VirtioStatusQUnlock(q);
}

VOID
VirtioStatusQSetActive(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN Active)
{
    if (StatusQ == NULL) {
        return;
    }

    VirtioStatusQLock(StatusQ);
    StatusQ->Active = Active;
    if (!Active) {
        StatusQ->PendingValid = FALSE;
    }
    VirtioStatusQUnlock(StatusQ);
}

VOID
VirtioStatusQSetDropOnFull(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN DropOnFull)
{
    if (StatusQ == NULL) {
        return;
    }

    VirtioStatusQLock(StatusQ);
    StatusQ->DropOnFull = DropOnFull;
    VirtioStatusQUnlock(StatusQ);
}

VOID
VirtioStatusQSetKeyboardLedSupportedMask(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedSupportedMask)
{
    if (StatusQ == NULL) {
        return;
    }

    VirtioStatusQLock(StatusQ);
    StatusQ->KeyboardLedSupportedMask = (UCHAR)(LedSupportedMask & (UCHAR)VIOINPUT_STATUSQ_LED_MASK_ALL);
    VirtioStatusQUnlock(StatusQ);
}

NTSTATUS
VirtioStatusQWriteKeyboardLedReport(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedBitfield)
{
    PDEVICE_CONTEXT devCtx;

    if (StatusQ == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    devCtx = (StatusQ->Device != NULL) ? VirtioInputGetDeviceContext(StatusQ->Device) : NULL;
    VirtioStatusQLock(StatusQ);
    if (!StatusQ->Active) {
        VirtioStatusQUnlock(StatusQ);
        if (devCtx != NULL) {
            VioInputCounterInc(&devCtx->Counters.LedWritesDropped);
        }
        return STATUS_DEVICE_NOT_READY;
    }
    StatusQ->PendingLedBitfield = LedBitfield;
    StatusQ->PendingValid = TRUE;
    NTSTATUS st = VirtioStatusQTrySubmit(StatusQ);
    VirtioStatusQUnlock(StatusQ);
    return st;
}

VOID
VirtioStatusQProcessUsedBuffers(_In_ PVIRTIO_STATUSQ StatusQ)
{
    PVIRTIO_STATUSQ q;
    void* cookie;
    UINT32 len;
    PDEVICE_CONTEXT devCtx;

    q = StatusQ;
    if (q == NULL) {
        return;
    }

    VirtioStatusQLock(q);
    if (q->Vq == NULL) {
        VirtioStatusQUnlock(q);
        return;
    }

    devCtx = (q->Device != NULL) ? VirtioInputGetDeviceContext(q->Device) : NULL;

    for (;;) {
        NTSTATUS status;

        cookie = NULL;
        len = 0;

        status = VirtqSplitGetUsed(q->Vq, &cookie, &len);
        if (status == STATUS_NOT_FOUND) {
            break;
        }
        if (!NT_SUCCESS(status)) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq VirtqSplitGetUsed failed: %!STATUS!\n", status);
            break;
        }

        if (devCtx != NULL) {
            VioInputCounterInc(&devCtx->Counters.StatusQCompletions);
        }

        UNREFERENCED_PARAMETER(len);

        if (cookie == NULL) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq completion cookie NULL\n");
        } else {
            UINT16 idx;
            if (VirtioStatusQCookieToIndex(q->TxVa, (size_t)q->TxBufferStride, q->TxBufferCount, cookie, &idx)) {
                VirtioStatusQPushFreeTxBuffer(q, idx);
            } else {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq completion cookie invalid\n");
            }
        }

        (VOID)VirtioStatusQTrySubmit(q);
    }

    VirtioStatusQUpdateDepthCounter(q);

    VirtioStatusQUnlock(q);
}

#endif /* _WIN32 */
