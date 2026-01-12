/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"

#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004
#endif

typedef struct _VIRTIOSND_EVENTQ_DRAIN_CONTEXT {
    PVIRTIOSND_DEVICE_EXTENSION Dx;
    ULONG Reposted;
} VIRTIOSND_EVENTQ_DRAIN_CONTEXT, *PVIRTIOSND_EVENTQ_DRAIN_CONTEXT;

static VOID VirtIoSndIntxDrainEventqUsed(
    _In_ USHORT QueueIndex,
    _In_opt_ void* Cookie,
    _In_ UINT32 UsedLen,
    _In_opt_ void* Context)
{
    PVIRTIOSND_EVENTQ_DRAIN_CONTEXT ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG_PTR poolBase;
    ULONG_PTR poolEnd;
    ULONG_PTR cookiePtr;
    ULONG_PTR off;
    VIRTIOSND_SG sg;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(QueueIndex);

    ctx = (PVIRTIOSND_EVENTQ_DRAIN_CONTEXT)Context;
    if (ctx == NULL) {
        return;
    }

    dx = ctx->Dx;
    if (dx == NULL) {
        return;
    }

    /*
     * Contract v1 defines no event messages; ignore contents.
     *
     * Still drain used entries to avoid ring space leaks if a future device model
     * starts emitting events (or if a buggy device completes event buffers).
     */
    if (Cookie == NULL) {
        VIRTIOSND_TRACE_ERROR("eventq completion with NULL cookie (len=%lu)\n", (ULONG)UsedLen);
        return;
    }

    if (dx->Removed) {
        /*
         * On surprise removal avoid MMIO accesses; do not repost/kick.
         * Best-effort draining is still useful to keep queue state consistent.
         */
        return;
    }

    if (dx->EventqBufferPool.Va == NULL || dx->EventqBufferPool.DmaAddr == 0 || dx->EventqBufferPool.Size == 0) {
        VIRTIOSND_TRACE_ERROR("eventq completion but buffer pool is not initialized (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        return;
    }

    poolBase = (ULONG_PTR)dx->EventqBufferPool.Va;
    poolEnd = poolBase + (ULONG_PTR)dx->EventqBufferPool.Size;
    cookiePtr = (ULONG_PTR)Cookie;

    if (cookiePtr < poolBase || cookiePtr >= poolEnd) {
        VIRTIOSND_TRACE_ERROR("eventq completion cookie out of range (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        return;
    }

    /* Ensure cookie points at the start of one of our fixed-size buffers. */
    off = cookiePtr - poolBase;
    if ((off % (ULONG_PTR)VIRTIOSND_EVENTQ_BUFFER_SIZE) != 0) {
        VIRTIOSND_TRACE_ERROR("eventq completion cookie misaligned (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        return;
    }

    if (off + (ULONG_PTR)VIRTIOSND_EVENTQ_BUFFER_SIZE > poolEnd - poolBase) {
        VIRTIOSND_TRACE_ERROR("eventq completion cookie range overflow (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        return;
    }

    if (UsedLen > (UINT32)VIRTIOSND_EVENTQ_BUFFER_SIZE) {
        /* Device bug: used length should never exceed posted writable capacity. */
        VIRTIOSND_TRACE_ERROR(
            "eventq completion length too large: %lu > %u (cookie=%p)\n",
            (ULONG)UsedLen,
            (UINT)VIRTIOSND_EVENTQ_BUFFER_SIZE,
            Cookie);
    }

    sg.addr = dx->EventqBufferPool.DmaAddr + (UINT64)off;
    sg.len = (UINT32)VIRTIOSND_EVENTQ_BUFFER_SIZE;
    sg.write = TRUE;

    status = VirtioSndQueueSubmit(&dx->Queues[VIRTIOSND_QUEUE_EVENT], &sg, 1, Cookie);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("eventq repost failed: 0x%08X (cookie=%p)\n", (UINT)status, Cookie);
        return;
    }

    ctx->Reposted++;
}

static BOOLEAN VirtIoSndIntxIsSharedInterrupt(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR *Desc)
{
    /*
     * CM_SHARE_DISPOSITION enum member names differ across WDK versions
     * (CmResourceShareShared vs CmShareShared), but the numeric value for "shared"
     * has been stable (3). Compare by value for portability.
     */
    return (Desc->ShareDisposition == 3) ? TRUE : FALSE;
}

static VOID VirtIoSndIntxQueueUsed(
    _In_ USHORT QueueIndex,
    _In_opt_ void *Cookie,
    _In_ UINT32 UsedLen,
    _In_opt_ void *Context)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Context;
    if (dx == NULL) {
        return;
    }

    switch (QueueIndex) {
    case VIRTIOSND_QUEUE_CONTROL:
        VirtioSndCtrlOnUsed(&dx->Control, Cookie, UsedLen);
        break;
    case VIRTIOSND_QUEUE_TX:
        if (dx->Tx.Queue != NULL && dx->Tx.Buffers != NULL) {
            VirtioSndTxOnUsed(&dx->Tx, Cookie, UsedLen);
        } else {
            VIRTIOSND_TRACE_ERROR("txq unexpected completion: cookie=%p len=%lu\n", Cookie, (ULONG)UsedLen);
        }
        break;
    case VIRTIOSND_QUEUE_RX:
        if (dx->Rx.Queue != NULL && dx->Rx.Requests != NULL) {
            VirtIoSndRxOnUsed(&dx->Rx, Cookie, UsedLen);
        } else {
            VIRTIOSND_TRACE_ERROR("rxq unexpected completion: cookie=%p len=%lu\n", Cookie, (ULONG)UsedLen);
        }
        break;
    default:
        UNREFERENCED_PARAMETER(Cookie);
        UNREFERENCED_PARAMETER(UsedLen);
        break;
    }
}

static VOID VirtIoSndIntxQueueWork(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    VIRTIOSND_EVENTQ_DRAIN_CONTEXT eventqDrain;

    UNREFERENCED_PARAMETER(Intx);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Cookie;
    if (dx == NULL) {
        return;
    }

    /*
     * INTx doesn't identify which queue fired; drain the queues that may have
     * in-flight cookies.
     */
    eventqDrain.Dx = dx;
    eventqDrain.Reposted = 0;
    VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_EVENT], VirtIoSndIntxDrainEventqUsed, &eventqDrain);
    if (eventqDrain.Reposted != 0 && !dx->Removed) {
        VirtioSndQueueKick(&dx->Queues[VIRTIOSND_QUEUE_EVENT]);
    }

    VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_CONTROL], VirtIoSndIntxQueueUsed, dx);

    /*
     * txq completions are only meaningful once the TX engine is initialized.
     * Draining txq early would pop used entries and lose cookies.
     */
    if (InterlockedCompareExchange(&dx->TxEngineInitialized, 0, 0) != 0 && dx->Tx.Queue != NULL && dx->Tx.Buffers != NULL) {
        VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_TX], VirtIoSndIntxQueueUsed, dx);
    }

    if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) != 0 && dx->Rx.Queue != NULL && dx->Rx.Requests != NULL) {
        VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_RX], VirtIoSndIntxQueueUsed, dx);
    }
}

static VOID VirtIoSndIntxConfigChange(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;

    UNREFERENCED_PARAMETER(Intx);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Cookie;
    if (dx == NULL || dx->Removed || dx->Transport.CommonCfg == NULL) {
        return;
    }

    /* Best-effort acknowledgement: read config_generation. */
    (VOID)READ_REGISTER_UCHAR((volatile UCHAR *)&dx->Transport.CommonCfg->config_generation);
}

_Use_decl_annotations_
VOID VirtIoSndIntxInitialize(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    RtlZeroMemory(&Dx->Intx, sizeof(Dx->Intx));
    RtlZeroMemory(&Dx->InterruptDesc, sizeof(Dx->InterruptDesc));
    Dx->InterruptDescPresent = FALSE;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndIntxCaptureResources(PVIRTIOSND_DEVICE_EXTENSION Dx, PCM_RESOURCE_LIST TranslatedResources)
{
    ULONG listIndex;
    BOOLEAN sawMessageInterrupt;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    Dx->InterruptDescPresent = FALSE;
    RtlZeroMemory(&Dx->InterruptDesc, sizeof(Dx->InterruptDesc));

    if (TranslatedResources == NULL || TranslatedResources->Count == 0) {
        return STATUS_RESOURCE_TYPE_NOT_FOUND;
    }

    sawMessageInterrupt = FALSE;

    for (listIndex = 0; listIndex < TranslatedResources->Count; ++listIndex) {
        PCM_FULL_RESOURCE_DESCRIPTOR full = &TranslatedResources->List[listIndex];
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = full->PartialResourceList.PartialDescriptors;
        ULONG count = full->PartialResourceList.Count;
        ULONG i;

        for (i = 0; i < count; ++i) {
            BOOLEAN shared;

            if (desc[i].Type != CmResourceTypeInterrupt) {
                continue;
            }

            /* Contract v1 requires line-based INTx; ignore message-signaled interrupts. */
            if ((desc[i].Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) {
                sawMessageInterrupt = TRUE;
                continue;
            }

            Dx->InterruptDesc = desc[i];
            Dx->InterruptDescPresent = TRUE;

            shared = VirtIoSndIntxIsSharedInterrupt(&Dx->InterruptDesc);

            VIRTIOSND_TRACE(
                "INTx resource: vector=%lu level=%lu affinity=%I64x mode=%s share=%u\n",
                Dx->InterruptDesc.u.Interrupt.Vector,
                Dx->InterruptDesc.u.Interrupt.Level,
                (ULONGLONG)Dx->InterruptDesc.u.Interrupt.Affinity,
                ((Dx->InterruptDesc.Flags & CM_RESOURCE_INTERRUPT_LATCHED) != 0) ? "latched" : "level",
                (UINT)shared);

            return STATUS_SUCCESS;
        }
    }

    return sawMessageInterrupt ? STATUS_NOT_SUPPORTED : STATUS_RESOURCE_TYPE_NOT_FOUND;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndIntxConnect(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!Dx->InterruptDescPresent) {
        return STATUS_RESOURCE_TYPE_NOT_FOUND;
    }

    if (Dx->Transport.IsrStatus == NULL) {
        /*
         * Without the ISR register mapping, an INTx interrupt would be impossible
         * to acknowledge/deassert and would result in an interrupt storm.
         */
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Dx->Intx.InterruptObject != NULL) {
        return STATUS_ALREADY_REGISTERED;
    }

    status = VirtioIntxConnect(Dx->Self,
                               &Dx->InterruptDesc,
                               Dx->Transport.IsrStatus,
                               VirtIoSndIntxConfigChange,
                               VirtIoSndIntxQueueWork,
                               NULL,
                               Dx,
                               &Dx->Intx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtioIntxConnect failed: 0x%08X\n", (UINT)status);
        return status;
    }

    VIRTIOSND_TRACE("INTx connected\n");
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID VirtIoSndIntxDisconnect(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    VirtioIntxDisconnect(&Dx->Intx);
}
