#include "virtio_statusq.h"

#include <ntddk.h>
#include <wdf.h>

#include "virtio_input.h"
#include "virtio_input_proto.h"

typedef struct _VIRTIO_STATUSQ {
    WDFDEVICE Device;
    struct virtqueue* Vq;
    WDFSPINLOCK Lock;

    BOOLEAN Active;
    BOOLEAN DropOnFull;

    BOOLEAN InFlight;
    BOOLEAN PendingValid;
    UCHAR PendingLedBitfield;

    VIRTIO_INPUT_EVENT EventBuffer[6];
} VIRTIO_STATUSQ, *PVIRTIO_STATUSQ;

typedef struct _VIOINPUT_SG_ENTRY {
    PVOID Buffer;
    ULONG Length;
} VIOINPUT_SG_ENTRY, *PVIOINPUT_SG_ENTRY;

// virtqueue API is provided by the virtio-win support library.
int virtqueue_add_buf(struct virtqueue* Vq, PVOID Sg, unsigned int OutNum, unsigned int InNum, void* Data, int Gfp);
void virtqueue_kick(struct virtqueue* Vq);
void* virtqueue_get_buf(struct virtqueue* Vq, unsigned int* Len);
void* virtqueue_detach_unused_buf(struct virtqueue* Vq);

#define VIOINPUT_GFP_ATOMIC 0

static ULONG
VirtioStatusQBuildLedEvents(_In_ UCHAR LedBitfield, _Out_writes_(6) VIRTIO_INPUT_EVENT* Events)
{
    ULONG eventCount = 0;

    Events[eventCount].Type = VIRTIO_INPUT_EV_LED;
    Events[eventCount].Code = VIRTIO_INPUT_LED_NUML;
    Events[eventCount].Value = (LedBitfield & 0x01) ? 1 : 0;
    eventCount++;

    Events[eventCount].Type = VIRTIO_INPUT_EV_LED;
    Events[eventCount].Code = VIRTIO_INPUT_LED_CAPSL;
    Events[eventCount].Value = (LedBitfield & 0x02) ? 1 : 0;
    eventCount++;

    Events[eventCount].Type = VIRTIO_INPUT_EV_LED;
    Events[eventCount].Code = VIRTIO_INPUT_LED_SCROLLL;
    Events[eventCount].Value = (LedBitfield & 0x04) ? 1 : 0;
    eventCount++;

    Events[eventCount].Type = VIRTIO_INPUT_EV_LED;
    Events[eventCount].Code = VIRTIO_INPUT_LED_COMPOSE;
    Events[eventCount].Value = (LedBitfield & 0x08) ? 1 : 0;
    eventCount++;

    Events[eventCount].Type = VIRTIO_INPUT_EV_LED;
    Events[eventCount].Code = VIRTIO_INPUT_LED_KANA;
    Events[eventCount].Value = (LedBitfield & 0x10) ? 1 : 0;
    eventCount++;

    Events[eventCount].Type = VIRTIO_INPUT_EV_SYN;
    Events[eventCount].Code = VIRTIO_INPUT_SYN_REPORT;
    Events[eventCount].Value = 0;
    eventCount++;

    return eventCount;
}

static NTSTATUS
VirtioStatusQTrySubmitLocked(_In_ PVIRTIO_STATUSQ StatusQ)
{
    ULONG eventCount;
    ULONG bytes;
    int rc;
    VIOINPUT_SG_ENTRY sg;

    if (!StatusQ->Active || StatusQ->Vq == NULL) {
        return STATUS_DEVICE_NOT_READY;
    }

    if (StatusQ->InFlight || !StatusQ->PendingValid) {
        return STATUS_SUCCESS;
    }

    eventCount = VirtioStatusQBuildLedEvents(StatusQ->PendingLedBitfield, StatusQ->EventBuffer);
    bytes = eventCount * sizeof(StatusQ->EventBuffer[0]);

    sg.Buffer = StatusQ->EventBuffer;
    sg.Length = bytes;

    rc = virtqueue_add_buf(StatusQ->Vq, &sg, 1, 0, StatusQ, VIOINPUT_GFP_ATOMIC);
    if (rc < 0) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "statusq virtqueue_add_buf failed rc=%d dropOnFull=%u\n",
            rc,
            StatusQ->DropOnFull);
        if (StatusQ->DropOnFull) {
            StatusQ->PendingValid = FALSE;
        }
        return STATUS_SUCCESS;
    }

    StatusQ->PendingValid = FALSE;
    StatusQ->InFlight = TRUE;
    virtqueue_kick(StatusQ->Vq);

    {
        PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(StatusQ->Device);
        VioInputCounterSet(&devCtx->Counters.VirtioQueueDepth, 1);
        VioInputCounterMaxUpdate(&devCtx->Counters.VirtioQueueMaxDepth, 1);
    }

    return STATUS_SUCCESS;
}

NTSTATUS
VirtioStatusQInitialize(_Out_ PVIRTIO_STATUSQ* StatusQ, _In_ WDFDEVICE Device, _In_ struct virtqueue* Vq)
{
    NTSTATUS status;
    PVIRTIO_STATUSQ q;
    WDF_OBJECT_ATTRIBUTES attributes;

    *StatusQ = NULL;

    q = (PVIRTIO_STATUSQ)ExAllocatePoolWithTag(NonPagedPool, sizeof(*q), 'qSoV');
    if (q == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(q, sizeof(*q));
    q->Device = Device;
    q->Vq = Vq;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = Device;
    status = WdfSpinLockCreate(&attributes, &q->Lock);
    if (!NT_SUCCESS(status)) {
        ExFreePoolWithTag(q, 'qSoV');
        return status;
    }

    *StatusQ = q;
    return STATUS_SUCCESS;
}

VOID
VirtioStatusQUninitialize(_In_ PVIRTIO_STATUSQ StatusQ)
{
    if (StatusQ == NULL) {
        return;
    }

    VirtioStatusQSetActive(StatusQ, FALSE);
    if (StatusQ->Lock != NULL) {
        WdfObjectDelete(StatusQ->Lock);
    }
    ExFreePoolWithTag(StatusQ, 'qSoV');
}

VOID
VirtioStatusQSetActive(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN Active)
{
    if (StatusQ == NULL) {
        return;
    }

    WdfSpinLockAcquire(StatusQ->Lock);
    StatusQ->Active = Active;
    StatusQ->InFlight = FALSE;
    StatusQ->PendingValid = FALSE;
    WdfSpinLockRelease(StatusQ->Lock);

    if (!Active && StatusQ->Vq != NULL) {
        while (virtqueue_detach_unused_buf(StatusQ->Vq) != NULL) {
        }
    }
}

VOID
VirtioStatusQSetDropOnFull(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN DropOnFull)
{
    if (StatusQ == NULL) {
        return;
    }

    WdfSpinLockAcquire(StatusQ->Lock);
    StatusQ->DropOnFull = DropOnFull;
    WdfSpinLockRelease(StatusQ->Lock);
}

NTSTATUS
VirtioStatusQWriteKeyboardLedReport(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedBitfield)
{
    NTSTATUS status;

    if (StatusQ == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    WdfSpinLockAcquire(StatusQ->Lock);

    if (!StatusQ->Active) {
        WdfSpinLockRelease(StatusQ->Lock);
        return STATUS_DEVICE_NOT_READY;
    }

    StatusQ->PendingLedBitfield = LedBitfield;
    StatusQ->PendingValid = TRUE;
    status = VirtioStatusQTrySubmitLocked(StatusQ);

    WdfSpinLockRelease(StatusQ->Lock);

    return status;
}

VOID
VirtioStatusQProcessUsedBuffers(_In_ PVIRTIO_STATUSQ StatusQ)
{
    unsigned int len;
    void* data;

    if (StatusQ == NULL || StatusQ->Vq == NULL) {
        return;
    }

    while ((data = virtqueue_get_buf(StatusQ->Vq, &len)) != NULL) {
        UNREFERENCED_PARAMETER(data);
        UNREFERENCED_PARAMETER(len);

        WdfSpinLockAcquire(StatusQ->Lock);
        StatusQ->InFlight = FALSE;
        {
            PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(StatusQ->Device);
            VioInputCounterSet(&devCtx->Counters.VirtioQueueDepth, 0);
        }
        VirtioStatusQTrySubmitLocked(StatusQ);
        WdfSpinLockRelease(StatusQ->Lock);
    }
}
