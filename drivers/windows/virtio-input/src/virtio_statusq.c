#include "virtio_statusq.h"

#include "virtqueue_split.h"

#include <ntddk.h>
#include <wdf.h>

#include "virtio_input.h"
#include "virtio_input_proto.h"

#define VIOINPUT_STATUSQ_POOL_TAG 'qSoV'

enum { VIOINPUT_STATUSQ_EVENTS_PER_BUFFER = 6 };

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

    BOOLEAN Active;
    BOOLEAN DropOnFull;

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

static __forceinline BOOLEAN VirtioStatusQCookieToIndex(_In_ const VIRTIO_STATUSQ* Q, _In_ PVOID Cookie, _Out_ UINT16* IndexOut)
{
    PUCHAR base;
    PUCHAR p;
    SIZE_T off;
    SIZE_T idx;

    if (Q == NULL || Cookie == NULL || IndexOut == NULL || Q->TxVa == NULL || Q->TxBufferStride == 0) {
        return FALSE;
    }

    base = (PUCHAR)Q->TxVa;
    p = (PUCHAR)Cookie;
    if (p < base) {
        return FALSE;
    }

    off = (SIZE_T)(p - base);
    if ((off % (SIZE_T)Q->TxBufferStride) != 0) {
        return FALSE;
    }

    idx = off / (SIZE_T)Q->TxBufferStride;
    if (idx >= Q->TxBufferCount) {
        return FALSE;
    }

    *IndexOut = (UINT16)idx;
    return TRUE;
}

static ULONG VirtioStatusQBuildLedEvents(_In_ UCHAR LedBitfield, _Out_writes_(VIOINPUT_STATUSQ_EVENTS_PER_BUFFER) struct virtio_input_event_le* Events)
{
    ULONG eventCount = 0;

    Events[eventCount].type = (uint16_t)VIRTIO_INPUT_EV_LED;
    Events[eventCount].code = (uint16_t)VIRTIO_INPUT_LED_NUML;
    Events[eventCount].value = (uint32_t)((LedBitfield & 0x01) ? 1 : 0);
    eventCount++;

    Events[eventCount].type = (uint16_t)VIRTIO_INPUT_EV_LED;
    Events[eventCount].code = (uint16_t)VIRTIO_INPUT_LED_CAPSL;
    Events[eventCount].value = (uint32_t)((LedBitfield & 0x02) ? 1 : 0);
    eventCount++;

    Events[eventCount].type = (uint16_t)VIRTIO_INPUT_EV_LED;
    Events[eventCount].code = (uint16_t)VIRTIO_INPUT_LED_SCROLLL;
    Events[eventCount].value = (uint32_t)((LedBitfield & 0x04) ? 1 : 0);
    eventCount++;

    Events[eventCount].type = (uint16_t)VIRTIO_INPUT_EV_LED;
    Events[eventCount].code = (uint16_t)VIRTIO_INPUT_LED_COMPOSE;
    Events[eventCount].value = (uint32_t)((LedBitfield & 0x08) ? 1 : 0);
    eventCount++;

    Events[eventCount].type = (uint16_t)VIRTIO_INPUT_EV_LED;
    Events[eventCount].code = (uint16_t)VIRTIO_INPUT_LED_KANA;
    Events[eventCount].value = (uint32_t)((LedBitfield & 0x10) ? 1 : 0);
    eventCount++;

    Events[eventCount].type = (uint16_t)VIRTIO_INPUT_EV_SYN;
    Events[eventCount].code = (uint16_t)VIRTIO_INPUT_SYN_REPORT;
    Events[eventCount].value = 0;
    eventCount++;

    return eventCount;
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

static __forceinline UINT16 VirtioStatusQPopFreeTxBuffer(_Inout_ PVIRTIO_STATUSQ Q)
{
    UINT16 idx;
    if (Q->FreeCount == 0 || Q->FreeHead == VIRTQ_SPLIT_NO_DESC) {
        return VIRTQ_SPLIT_NO_DESC;
    }

    idx = Q->FreeHead;
    Q->FreeHead = Q->NextFree[idx];
    Q->NextFree[idx] = VIRTQ_SPLIT_NO_DESC;
    Q->FreeCount--;
    return idx;
}

static __forceinline VOID VirtioStatusQPushFreeTxBuffer(_Inout_ PVIRTIO_STATUSQ Q, _In_ UINT16 Index)
{
    Q->NextFree[Index] = Q->FreeHead;
    Q->FreeHead = Index;
    Q->FreeCount++;
}

static NTSTATUS VirtioStatusQTrySubmitLocked(_Inout_ PVIRTIO_STATUSQ Q)
{
    UINT16 idx;
    PUCHAR bufVa;
    UINT64 bufPa;
    ULONG eventCount;
    UINT32 bytes;
    VIRTQ_SG sg;
    UINT16 head;
    NTSTATUS status;

    if (Q == NULL || Q->PciDevice == NULL || Q->Vq == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!Q->Active || !Q->PendingValid) {
        return STATUS_SUCCESS;
    }

    idx = VirtioStatusQPopFreeTxBuffer(Q);
    if (idx == VIRTQ_SPLIT_NO_DESC) {
        if (Q->DropOnFull) {
            Q->PendingValid = FALSE;
        }
        return STATUS_SUCCESS;
    }

    bufVa = VirtioStatusQTxBufVa(Q, idx);
    bufPa = VirtioStatusQTxBufPa(Q, idx);

    eventCount = VirtioStatusQBuildLedEvents(Q->PendingLedBitfield, (struct virtio_input_event_le*)bufVa);
    bytes = (UINT32)(eventCount * sizeof(struct virtio_input_event_le));

    sg.addr = bufPa;
    sg.len = bytes;
    sg.write = FALSE;

    head = VIRTQ_SPLIT_NO_DESC;
    status = VirtqSplitAddBuffer(Q->Vq, &sg, 1, bufVa, &head);
    if (!NT_SUCCESS(status)) {
        VirtioStatusQPushFreeTxBuffer(Q, idx);
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq VirtqSplitAddBuffer failed: %!STATUS!\n", status);
        if (Q->DropOnFull) {
            Q->PendingValid = FALSE;
        }
        return STATUS_SUCCESS;
    }

    Q->PendingValid = FALSE;

    VirtqSplitPublish(Q->Vq, head);
    VirtioPciNotifyQueue(Q->PciDevice, Q->QueueIndex);
    VirtqSplitKickCommit(Q->Vq);

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
    status = WdfCommonBufferCreate(DmaEnabler, ringBytes, &attributes, &q->RingCommonBuffer);
    if (!NT_SUCCESS(status)) {
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
        ExFreePoolWithTag(q->Vq, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q->NextFree, VIOINPUT_STATUSQ_POOL_TAG);
        ExFreePoolWithTag(q, VIOINPUT_STATUSQ_POOL_TAG);
        return status;
    }

    txBytes = (SIZE_T)q->TxBufferStride * (SIZE_T)q->TxBufferCount;
    status = WdfCommonBufferCreate(DmaEnabler, txBytes, &attributes, &q->TxCommonBuffer);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(q->RingCommonBuffer);
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
}

VOID
VirtioStatusQGetRingAddresses(_In_ PVIRTIO_STATUSQ StatusQ, _Out_ UINT64* DescPa, _Out_ UINT64* AvailPa, _Out_ UINT64* UsedPa)
{
    if (DescPa != NULL) {
        *DescPa = 0;
    }
    if (AvailPa != NULL) {
        *AvailPa = 0;
    }
    if (UsedPa != NULL) {
        *UsedPa = 0;
    }

    if (StatusQ == NULL || StatusQ->Vq == NULL) {
        return;
    }

    if (DescPa != NULL) {
        *DescPa = StatusQ->Vq->desc_pa;
    }
    if (AvailPa != NULL) {
        *AvailPa = StatusQ->Vq->avail_pa;
    }
    if (UsedPa != NULL) {
        *UsedPa = StatusQ->Vq->used_pa;
    }
}

VOID
VirtioStatusQSetActive(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN Active)
{
    if (StatusQ == NULL) {
        return;
    }

    StatusQ->Active = Active;
    if (!Active) {
        StatusQ->PendingValid = FALSE;
    }
}

VOID
VirtioStatusQSetDropOnFull(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN DropOnFull)
{
    if (StatusQ == NULL) {
        return;
    }

    StatusQ->DropOnFull = DropOnFull;
}

NTSTATUS
VirtioStatusQWriteKeyboardLedReport(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedBitfield)
{
    if (StatusQ == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!StatusQ->Active) {
        return STATUS_DEVICE_NOT_READY;
    }

    StatusQ->PendingLedBitfield = LedBitfield;
    StatusQ->PendingValid = TRUE;
    return VirtioStatusQTrySubmitLocked(StatusQ);
}

VOID
VirtioStatusQProcessUsedBuffers(_In_ PVIRTIO_STATUSQ StatusQ)
{
    PVIRTIO_STATUSQ q;
    void* cookie;
    UINT32 len;

    q = StatusQ;
    if (q == NULL || q->Vq == NULL) {
        return;
    }

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

        UNREFERENCED_PARAMETER(len);

        if (cookie != NULL) {
            UINT16 idx;
            if (VirtioStatusQCookieToIndex(q, cookie, &idx)) {
                VirtioStatusQPushFreeTxBuffer(q, idx);
            } else {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "statusq completion cookie invalid\n");
            }
        }

        (VOID)VirtioStatusQTrySubmitLocked(q);
    }

    VirtioStatusQUpdateDepthCounter(q);
}

