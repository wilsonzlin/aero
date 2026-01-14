/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "topology.h"
#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"

#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004
#endif

#ifndef CONNECT_MESSAGE_BASED
/*
 * Some older WDK header sets omit the CONNECT_MESSAGE_BASED definition even
 * though IoConnectInterruptEx supports message-based interrupts on Vista+.
 *
 * The documented value is 2.
 */
#define CONNECT_MESSAGE_BASED 0x2
#endif

#ifndef DISCONNECT_MESSAGE_BASED
/* Some WDKs use DISCONNECT_MESSAGE_BASED for IoDisconnectInterruptEx; others reuse CONNECT_MESSAGE_BASED. */
#define DISCONNECT_MESSAGE_BASED CONNECT_MESSAGE_BASED
#endif

typedef struct _VIRTIOSND_EVENTQ_DRAIN_CONTEXT {
    PVIRTIOSND_DEVICE_EXTENSION Dx;
    ULONGLONG RepostMask;
} VIRTIOSND_EVENTQ_DRAIN_CONTEXT, *PVIRTIOSND_EVENTQ_DRAIN_CONTEXT;

static BOOLEAN VirtIoSndMessageIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext, _In_ ULONG MessageID);
static VOID VirtIoSndMessageDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2);

static __forceinline BOOLEAN VirtIoSndIntxIsSharedInterrupt(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR *Desc)
{
    /*
     * CM_SHARE_DISPOSITION enum member names differ across WDK versions
     * (CmResourceShareShared vs CmShareShared), but the numeric value for "shared"
     * has been stable (3). Compare by value for portability.
     */
    return (Desc->ShareDisposition == 3) ? TRUE : FALSE;
}

/*
 * eventq contents are device-controlled; keep error logging rate-limited even in
 * free builds.
 */
static __forceinline BOOLEAN VirtIoSndShouldRateLimitLog(_Inout_ volatile LONG* Counter)
{
    /*
     * Log the 1st occurrence and then every 256th.
     */
    LONG n;
    if (Counter == NULL) {
        return TRUE;
    }
    n = InterlockedIncrement(Counter);
    return ((n & 0xFF) == 1) ? TRUE : FALSE;
}

static volatile LONG g_eventqErrorLog;

static BOOLEAN VirtIoSndEventqSignalStreamNotificationThunk(_In_opt_ void* Context, _In_ ULONG StreamId)
{
    return VirtIoSndEventqSignalStreamNotificationEvent((PVIRTIOSND_DEVICE_EXTENSION)Context, StreamId);
}

static VOID VirtIoSndDrainEventqUsed(_In_ USHORT QueueIndex, _In_opt_ void *Cookie, _In_ UINT32 UsedLen, _In_opt_ void *Context)
{
    PVIRTIOSND_EVENTQ_DRAIN_CONTEXT ctx;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cb;
    VIRTIOSND_EVENTQ_PERIOD_STATE period;

    UNREFERENCED_PARAMETER(QueueIndex);

    ctx = (PVIRTIOSND_EVENTQ_DRAIN_CONTEXT)Context;
    if (ctx == NULL) {
        return;
    }

    dx = ctx->Dx;
    if (dx == NULL) {
        return;
    }

    cb.Lock = &dx->EventqLock;
    cb.Callback = &dx->EventqCallback;
    cb.CallbackContext = &dx->EventqCallbackContext;
    cb.CallbackInFlight = &dx->EventqCallbackInFlight;

    RtlZeroMemory(&period, sizeof(period));
    period.SignalStreamNotification = VirtIoSndEventqSignalStreamNotificationThunk;
    period.SignalStreamNotificationContext = dx;
    period.PcmPeriodSeq = dx->PcmPeriodSeq;
    period.PcmLastPeriodEventTime100ns = dx->PcmLastPeriodEventTime100ns;
    period.StreamCount = (ULONG)RTL_NUMBER_OF(dx->PcmPeriodSeq);

    (VOID)VirtIoSndEventqHandleUsed(&dx->Queues[VIRTIOSND_QUEUE_EVENT],
                                   &dx->EventqBufferPool,
                                   &dx->EventqStats,
                                   &dx->JackState,
                                   &cb,
                                   &period,
                                   dx->Started,
                                   dx->Removed,
                                   Cookie,
                                   UsedLen,
                                   /*EnableDebugLogs=*/TRUE,
                                   &ctx->RepostMask);
}

static VOID VirtIoSndAckConfigChange(_Inout_ PVIRTIOSND_DEVICE_EXTENSION dx)
{
    if (dx == NULL || dx->Removed || dx->Transport.CommonCfg == NULL) {
        return;
    }

    /* Best-effort acknowledgement: read config_generation. */
    (VOID)READ_REGISTER_UCHAR((volatile UCHAR *)&dx->Transport.CommonCfg->config_generation);
}

static VOID VirtIoSndQueueUsedDispatch(_In_ USHORT QueueIndex, _In_opt_ void *Cookie, _In_ UINT32 UsedLen, _In_opt_ void *Context)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Context;
    if (dx == NULL) {
        return;
    }

    switch (QueueIndex) {
    case VIRTIOSND_QUEUE_CONTROL:
        /*
         * MSI/MSI-X interrupts may be connected before StartHardware finishes
         * initializing protocol engines. Only deliver control completions once
         * the control engine is initialized.
         */
        if (dx->Control.DmaCtx != NULL) {
            VirtioSndCtrlOnUsed(&dx->Control, Cookie, UsedLen);
        } else {
            VIRTIOSND_TRACE_ERROR("controlq unexpected completion before engine init: cookie=%p len=%lu\n", Cookie, (ULONG)UsedLen);
        }
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

static __forceinline VOID VirtIoSndDrainQueue(_Inout_ PVIRTIOSND_DEVICE_EXTENSION dx, _In_ USHORT queueIndex)
{
    if (dx == NULL) {
        return;
    }

    if (queueIndex < VIRTIOSND_QUEUE_COUNT) {
        (VOID)InterlockedIncrement(&dx->QueueDrainCount[queueIndex]);
    }

    if (dx->Queues[queueIndex].Ops == NULL) {
        return;
    }

    switch (queueIndex) {
    case VIRTIOSND_QUEUE_EVENT:
    {
        VIRTIOSND_EVENTQ_DRAIN_CONTEXT eventqDrain;
        VIRTIOSND_SG sg;
        NTSTATUS status;
        ULONG reposted;
        ULONG i;

        eventqDrain.Dx = dx;
        eventqDrain.RepostMask = 0;
        VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_EVENT], VirtIoSndDrainEventqUsed, &eventqDrain);

        reposted = 0;
        if (eventqDrain.RepostMask != 0 && !dx->Removed &&
            dx->EventqBufferPool.Va != NULL && dx->EventqBufferPool.DmaAddr != 0 &&
            dx->EventqBufferCount != 0) {
            for (i = 0; i < dx->EventqBufferCount && i < 64u; ++i) {
                if ((eventqDrain.RepostMask & (1ull << i)) == 0) {
                    continue;
                }

                sg.addr = dx->EventqBufferPool.DmaAddr + ((UINT64)i * (UINT64)VIRTIOSND_EVENTQ_BUFFER_SIZE);
                sg.len = (UINT32)VIRTIOSND_EVENTQ_BUFFER_SIZE;
                sg.write = TRUE;

                status = VirtioSndQueueSubmit(&dx->Queues[VIRTIOSND_QUEUE_EVENT], &sg, 1,
                                              (PUCHAR)dx->EventqBufferPool.Va + ((SIZE_T)i * (SIZE_T)VIRTIOSND_EVENTQ_BUFFER_SIZE));
                if (NT_SUCCESS(status)) {
                    reposted++;
                } else if (VirtIoSndShouldRateLimitLog(&g_eventqErrorLog)) {
                    VIRTIOSND_TRACE_ERROR("eventq repost failed: 0x%08X (buf=%lu)\n", (UINT)status, i);
                }
            }
        }

        if (reposted != 0 && !dx->Removed) {
            VirtioSndQueueKick(&dx->Queues[VIRTIOSND_QUEUE_EVENT]);
        }
        break;
    }
    case VIRTIOSND_QUEUE_CONTROL:
        VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_CONTROL], VirtIoSndQueueUsedDispatch, dx);
        break;
    case VIRTIOSND_QUEUE_TX:
        if (InterlockedCompareExchange(&dx->TxEngineInitialized, 0, 0) != 0 && dx->Tx.Queue != NULL && dx->Tx.Buffers != NULL) {
            VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_TX], VirtIoSndQueueUsedDispatch, dx);
        }
        break;
    case VIRTIOSND_QUEUE_RX:
        if (InterlockedCompareExchange(&dx->RxEngineInitialized, 0, 0) != 0 && dx->Rx.Queue != NULL && dx->Rx.Requests != NULL) {
            VirtioSndQueueSplitDrainUsed(&dx->QueueSplit[VIRTIOSND_QUEUE_RX], VirtIoSndQueueUsedDispatch, dx);
        }
        break;
    default:
        break;
    }
}

static __forceinline VOID VirtIoSndDrainAllQueues(_Inout_ PVIRTIOSND_DEVICE_EXTENSION dx)
{
    if (dx == NULL) {
        return;
    }

    /* Contract v1 INTx does not identify which queue fired. */
    VirtIoSndDrainQueue(dx, VIRTIOSND_QUEUE_EVENT);
    VirtIoSndDrainQueue(dx, VIRTIOSND_QUEUE_CONTROL);
    VirtIoSndDrainQueue(dx, VIRTIOSND_QUEUE_TX);
    VirtIoSndDrainQueue(dx, VIRTIOSND_QUEUE_RX);
}

static VOID VirtIoSndIntxQueueWork(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;

    UNREFERENCED_PARAMETER(Intx);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Cookie;
    VirtIoSndDrainAllQueues(dx);
}

static VOID VirtIoSndIntxConfigChange(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;

    UNREFERENCED_PARAMETER(Intx);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)Cookie;
    VirtIoSndAckConfigChange(dx);
}

_Use_decl_annotations_
VOID VirtIoSndInterruptInitialize(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    /*
     * Reset topology jack state to the default (connected) at device start.
     *
     * If the device never emits jack events, this preserves historical behavior.
     * If the device does emit events, the first event will update the state.
     */
    VirtIoSndTopology_ResetJackState();

    /*
     * Eventq callback lock is used by both the INTx/MSI DPC path and by teardown
     * (StopHardware). Initialize it here so StopHardware can safely clear the
     * callback even on the first START_DEVICE, before StartHardware has fully
     * initialized the transport.
     */
    KeInitializeSpinLock(&Dx->EventqLock);
    Dx->EventqCallback = NULL;
    Dx->EventqCallbackContext = NULL;
    Dx->EventqCallbackInFlight = 0;

    RtlZeroMemory(&Dx->Intx, sizeof(Dx->Intx));
    RtlZeroMemory(&Dx->InterruptDesc, sizeof(Dx->InterruptDesc));
    Dx->InterruptDescPresent = FALSE;

    RtlZeroMemory(&Dx->MessageInterruptDesc, sizeof(Dx->MessageInterruptDesc));
    Dx->MessageInterruptDescPresent = FALSE;
    Dx->MessageInterruptsConnected = FALSE;
    Dx->MessageInterruptsActive = FALSE;

    Dx->MessageInterruptInfo = NULL;
    Dx->MessageInterruptConnectionContext = NULL;
    Dx->MessageInterruptCount = 0;

    RtlZeroMemory(&Dx->MessageDpc, sizeof(Dx->MessageDpc));
    Dx->MessageDpcInFlight = 0;
    Dx->MessagePendingMask = 0;
    Dx->MessageIsrCount = 0;
    Dx->MessageDpcCount = 0;

    Dx->MsixAllOnVector0 = TRUE;
    Dx->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    RtlZeroMemory(Dx->MsixQueueVectors, sizeof(Dx->MsixQueueVectors));

    RtlZeroMemory(Dx->QueueDrainCount, sizeof(Dx->QueueDrainCount));
    RtlZeroMemory(Dx->PcmPeriodSeq, sizeof(Dx->PcmPeriodSeq));
    RtlZeroMemory(Dx->PcmLastPeriodEventTime100ns, sizeof(Dx->PcmLastPeriodEventTime100ns));
}

_Use_decl_annotations_
NTSTATUS VirtIoSndInterruptCaptureResources(PVIRTIOSND_DEVICE_EXTENSION Dx, PCM_RESOURCE_LIST TranslatedResources)
{
    ULONG listIndex;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    Dx->InterruptDescPresent = FALSE;
    RtlZeroMemory(&Dx->InterruptDesc, sizeof(Dx->InterruptDesc));

    Dx->MessageInterruptDescPresent = FALSE;
    RtlZeroMemory(&Dx->MessageInterruptDesc, sizeof(Dx->MessageInterruptDesc));

    if (TranslatedResources == NULL || TranslatedResources->Count == 0) {
        return STATUS_RESOURCE_TYPE_NOT_FOUND;
    }

    for (listIndex = 0; listIndex < TranslatedResources->Count; ++listIndex) {
        PCM_FULL_RESOURCE_DESCRIPTOR full = &TranslatedResources->List[listIndex];
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = full->PartialResourceList.PartialDescriptors;
        ULONG count = full->PartialResourceList.Count;
        ULONG i;

        for (i = 0; i < count; ++i) {
            if (desc[i].Type != CmResourceTypeInterrupt) {
                continue;
            }

            if ((desc[i].Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) {
                if (!Dx->MessageInterruptDescPresent) {
                    Dx->MessageInterruptDesc = desc[i];
                    Dx->MessageInterruptDescPresent = TRUE;
                    VIRTIOSND_TRACE("MSI/MSI-X interrupt resource present (flags=0x%04X)\n", (UINT)Dx->MessageInterruptDesc.Flags);
                }
                continue;
            }

            if (!Dx->InterruptDescPresent) {
                BOOLEAN shared;
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
            }
        }
    }

    return (Dx->MessageInterruptDescPresent || Dx->InterruptDescPresent) ? STATUS_SUCCESS : STATUS_RESOURCE_TYPE_NOT_FOUND;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndInterruptConnectMessage(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;
    IO_CONNECT_INTERRUPT_PARAMETERS params;
    ULONG msgCount;
    ULONG usedVectorCount;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!Dx->MessageInterruptDescPresent) {
        return STATUS_RESOURCE_TYPE_NOT_FOUND;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Dx->MessageInterruptsConnected || Dx->MessageInterruptConnectionContext != NULL) {
        return STATUS_ALREADY_REGISTERED;
    }

    Dx->MessagePendingMask = 0;
    Dx->MessageDpcInFlight = 0;
    KeInitializeDpc(&Dx->MessageDpc, VirtIoSndMessageDpc, Dx);

    msgCount = (ULONG)Dx->MessageInterruptDesc.u.MessageInterrupt.MessageCount;
    if (msgCount == 0) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    usedVectorCount = 1;
    if (msgCount >= (ULONG)(1u + VIRTIOSND_QUEUE_COUNT)) {
        usedVectorCount = (ULONG)(1u + VIRTIOSND_QUEUE_COUNT);
    }

    RtlZeroMemory(&params, sizeof(params));
    params.Version = CONNECT_MESSAGE_BASED;
    params.MessageBased.PhysicalDeviceObject = Dx->Pdo;
    params.MessageBased.ServiceRoutine = VirtIoSndMessageIsr;
    params.MessageBased.ServiceContext = Dx;
    params.MessageBased.SpinLock = NULL;
    params.MessageBased.SynchronizeIrql = (ULONG)Dx->MessageInterruptDesc.u.MessageInterrupt.Level;
    params.MessageBased.FloatingSave = FALSE;
    params.MessageBased.MessageCount = usedVectorCount;
    params.MessageBased.MessageInfo = NULL;
    params.MessageBased.ConnectionContext = NULL;

    status = IoConnectInterruptEx(&params);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IoConnectInterruptEx(CONNECT_MESSAGE_BASED) failed: 0x%08X\n", (UINT)status);
        return status;
    }

    Dx->MessageInterruptInfo = params.MessageBased.MessageInfo;
    Dx->MessageInterruptConnectionContext = params.MessageBased.ConnectionContext;
    Dx->MessageInterruptCount = usedVectorCount;
    if (Dx->MessageInterruptInfo != NULL && Dx->MessageInterruptInfo->MessageCount != 0) {
        Dx->MessageInterruptCount = Dx->MessageInterruptInfo->MessageCount;
    }

    msgCount = Dx->MessageInterruptCount;
    if (msgCount == 0 || msgCount > 32) {
        VirtIoSndInterruptDisconnect(Dx);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    /* Message IDs are used directly as virtio MSI-X vector indices. */
    Dx->MsixConfigVector = 0;
    if (msgCount >= (ULONG)(1u + VIRTIOSND_QUEUE_COUNT)) {
        ULONG q;
        Dx->MsixAllOnVector0 = FALSE;
        for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
            Dx->MsixQueueVectors[q] = (USHORT)(q + 1u);
        }
    } else {
        ULONG q;
        Dx->MsixAllOnVector0 = TRUE;
        for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
            Dx->MsixQueueVectors[q] = 0;
        }
    }

    Dx->MessageInterruptsConnected = TRUE;
    Dx->MessageInterruptsActive = TRUE;

    VIRTIOSND_TRACE("MSI/MSI-X connected (messages=%lu, all_on_vector0=%u)\n", msgCount, Dx->MsixAllOnVector0 ? 1u : 0u);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndInterruptConnectIntx(PVIRTIOSND_DEVICE_EXTENSION Dx)
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

    Dx->MessageInterruptsActive = FALSE;

    VIRTIOSND_TRACE("INTx connected\n");
    return STATUS_SUCCESS;
}

static VOID VirtIoSndDisconnectMessageInternal(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    BOOLEAN removed;
    LONG remaining;
    LARGE_INTEGER delay;
    IO_DISCONNECT_INTERRUPT_PARAMETERS params;

    if (Dx == NULL) {
        return;
    }

    if (!Dx->MessageInterruptsConnected && Dx->MessageInterruptConnectionContext == NULL) {
        Dx->MessageInterruptsActive = FALSE;
        return;
    }

    Dx->MessageInterruptsActive = FALSE;
    Dx->MessageInterruptsConnected = FALSE;

    if (Dx->MessageInterruptConnectionContext != NULL) {
        RtlZeroMemory(&params, sizeof(params));
        params.Version = DISCONNECT_MESSAGE_BASED;
        params.MessageBased.ConnectionContext = Dx->MessageInterruptConnectionContext;
        IoDisconnectInterruptEx(&params);
    }

    Dx->MessageInterruptInfo = NULL;
    Dx->MessageInterruptConnectionContext = NULL;
    Dx->MessageInterruptCount = 0;

    /* Cancel any DPC that is queued but not yet running. */
    removed = KeRemoveQueueDpc(&Dx->MessageDpc);
    if (removed) {
        remaining = InterlockedDecrement(&Dx->MessageDpcInFlight);
        if (remaining < 0) {
            (VOID)InterlockedExchange(&Dx->MessageDpcInFlight, 0);
        }
    }

    /* Wait for any in-flight DPC to finish before callers free queues/unmap MMIO. */
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        delay.QuadPart = -10 * 1000; /* 1ms */
        for (;;) {
            remaining = InterlockedCompareExchange(&Dx->MessageDpcInFlight, 0, 0);
            if (remaining <= 0) {
                if (remaining < 0) {
                    (VOID)InterlockedExchange(&Dx->MessageDpcInFlight, 0);
                }
                break;
            }
            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    }

    Dx->MessagePendingMask = 0;
    Dx->MsixAllOnVector0 = TRUE;
    Dx->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    RtlZeroMemory(Dx->MsixQueueVectors, sizeof(Dx->MsixQueueVectors));
}

_Use_decl_annotations_
VOID VirtIoSndInterruptDisconnect(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    VirtIoSndDisconnectMessageInternal(Dx);
    VirtioIntxDisconnect(&Dx->Intx);
}

_Use_decl_annotations_
LONG VirtIoSndInterruptGetDpcInFlight(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    LONG intx;
    LONG msg;

    if (Dx == NULL) {
        return 0;
    }

    intx = InterlockedCompareExchange(&Dx->Intx.DpcInFlight, 0, 0);
    msg = InterlockedCompareExchange(&Dx->MessageDpcInFlight, 0, 0);
    return (intx > msg) ? intx : msg;
}

_Use_decl_annotations_
VOID VirtIoSndInterruptDisableDeviceVectors(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    ULONG q;

    if (Dx == NULL) {
        return;
    }

    if (!Dx->MessageInterruptsActive) {
        return;
    }

    if (Dx->Removed || Dx->Transport.CommonCfg == NULL) {
        return;
    }

    (void)VirtioPciModernTransportSetConfigMsixVector(&Dx->Transport, VIRTIO_PCI_MSI_NO_VECTOR);
    for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
        (void)VirtioPciModernTransportSetQueueMsixVector(&Dx->Transport, (USHORT)q, VIRTIO_PCI_MSI_NO_VECTOR);
    }
}

/*
 * PKMESSAGE_SERVICE_ROUTINE
 *
 * For MSI/MSI-X treat interrupts as non-shared and do not touch the virtio ISR
 * status register (INTx-only read-to-ack semantics).
 */
static BOOLEAN VirtIoSndMessageIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext, _In_ ULONG MessageID)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG mask;
    BOOLEAN inserted;

    UNREFERENCED_PARAMETER(Interrupt);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)ServiceContext;
    if (dx == NULL) {
        return FALSE;
    }

    if (!dx->MessageInterruptsConnected) {
        return TRUE;
    }

    (VOID)InterlockedIncrement(&dx->MessageIsrCount);

    mask = (MessageID < 32) ? (1u << MessageID) : 1u;
    (VOID)InterlockedOr(&dx->MessagePendingMask, (LONG)mask);

    (VOID)InterlockedIncrement(&dx->MessageDpcInFlight);
    inserted = KeInsertQueueDpc(&dx->MessageDpc, NULL, NULL);
    if (!inserted) {
        LONG remaining = InterlockedDecrement(&dx->MessageDpcInFlight);
        if (remaining < 0) {
            (VOID)InterlockedExchange(&dx->MessageDpcInFlight, 0);
        }
    }

    return TRUE;
}

/*
 * PKDEFERRED_ROUTINE
 */
static VOID VirtIoSndMessageDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG pending;
    ULONG msg;
    LONG remaining;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    dx = (PVIRTIOSND_DEVICE_EXTENSION)DeferredContext;
    if (dx == NULL) {
        return;
    }

    (VOID)InterlockedIncrement(&dx->MessageDpcCount);

    pending = (ULONG)InterlockedExchange(&dx->MessagePendingMask, 0);
    if (pending == 0) {
        goto out;
    }

    if (!dx->MessageInterruptsConnected) {
        goto out;
    }

    if (dx->MsixAllOnVector0) {
        VirtIoSndAckConfigChange(dx);
        VirtIoSndDrainAllQueues(dx);
        goto out;
    }

    for (msg = 0; pending != 0; ++msg) {
        if ((pending & 1u) != 0) {
            if (msg == 0) {
                VirtIoSndAckConfigChange(dx);
            } else if (msg >= 1u && msg < (1u + VIRTIOSND_QUEUE_COUNT)) {
                VirtIoSndDrainQueue(dx, (USHORT)(msg - 1u));
            }
        }
        pending >>= 1;
    }

out:
    remaining = InterlockedDecrement(&dx->MessageDpcInFlight);
    if (remaining < 0) {
        (VOID)InterlockedExchange(&dx->MessageDpcInFlight, 0);
    }
}
