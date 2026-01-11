/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"

#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004
#endif

static BOOLEAN VirtIoSndIntxIsSharedInterrupt(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR *Desc)
{
#if defined(CmResourceShareShared)
    return (Desc->ShareDisposition == CmResourceShareShared) ? TRUE : FALSE;
#elif defined(CmShareShared)
    return (Desc->ShareDisposition == CmShareShared) ? TRUE : FALSE;
#else
    UNREFERENCED_PARAMETER(Desc);
    return TRUE;
#endif
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
        if (InterlockedCompareExchange(&dx->TxEngineInitialized, 0, 0) != 0 && dx->Tx.Queue != NULL && dx->Tx.Buffers != NULL) {
            VirtioSndTxOnUsed(&dx->Tx, Cookie, UsedLen);
        } else {
            VIRTIOSND_TRACE_ERROR("txq unexpected completion: cookie=%p len=%lu\n", Cookie, (ULONG)UsedLen);
        }
        break;
    case VIRTIOSND_QUEUE_RX:
        if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) != 0 && dx->Rx.Queue != NULL && dx->Rx.Requests != NULL) {
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

    UNREFERENCED_PARAMETER(Intx);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Cookie;
    if (dx == NULL) {
        return;
    }

    /*
     * INTx doesn't identify which queue fired; drain the queues that may have
     * in-flight cookies.
     */
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
    if (dx == NULL || dx->Transport.CommonCfg == NULL) {
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
