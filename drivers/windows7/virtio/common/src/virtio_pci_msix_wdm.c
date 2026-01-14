/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtio_pci_msix_wdm.h"

/*
 * Pool tags are traditionally specified as multi-character constants (e.g. 'xIsV')
 * in WDK codebases. Host-side unit tests build this file with GCC/Clang, which
 * warn on multi-character character constants.
 *
 * Define the tag via a portable shift-based encoding for non-MSVC builds to
 * avoid -Wmultichar noise in CI.
 */
#if defined(_MSC_VER)
#define VIRTIO_MSIX_WDM_POOL_TAG 'xIsV'
#else
#define VIRTIO_MSIX_WDM_MAKE_POOL_TAG(a, b, c, d) \
    ((ULONG)(((ULONG)(a) << 24) | ((ULONG)(b) << 16) | ((ULONG)(c) << 8) | ((ULONG)(d))))
#define VIRTIO_MSIX_WDM_POOL_TAG VIRTIO_MSIX_WDM_MAKE_POOL_TAG('x', 'I', 's', 'V')
#endif

static BOOLEAN VirtioMsixIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext, _In_ ULONG MessageId);
static VOID VirtioMsixDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2);

static __forceinline ULONGLONG VirtioMsixQueueMaskAll(_In_ ULONG QueueCount)
{
    ULONGLONG mask;
    ULONG q;

    mask = 0;
    for (q = 0; q < QueueCount; q++) {
        mask |= (1ULL << q);
    }
    return mask;
}

static VOID VirtioMsixFreeAllocations(_Inout_ PVIRTIO_MSIX_WDM Msix)
{
    if (Msix == NULL) {
        return;
    }

    if (Msix->QueueVectors != NULL) {
        ExFreePoolWithTag(Msix->QueueVectors, VIRTIO_MSIX_WDM_POOL_TAG);
        Msix->QueueVectors = NULL;
    }

    if (Msix->QueueLocks != NULL) {
        ExFreePoolWithTag(Msix->QueueLocks, VIRTIO_MSIX_WDM_POOL_TAG);
        Msix->QueueLocks = NULL;
    }

    if (Msix->Vectors != NULL) {
        ExFreePoolWithTag(Msix->Vectors, VIRTIO_MSIX_WDM_POOL_TAG);
        Msix->Vectors = NULL;
    }
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioMsixConnect(_In_ PDEVICE_OBJECT DeviceObject,
                           _In_ PDEVICE_OBJECT PhysicalDeviceObject,
                           _In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDescTranslated,
                           _In_ ULONG QueueCount,
                           _In_opt_ PKSPIN_LOCK CommonCfgLock,
                           _In_opt_ EVT_VIRTIO_MSIX_CONFIG_CHANGE* EvtConfigChange,
                           _In_opt_ EVT_VIRTIO_MSIX_DRAIN_QUEUE* EvtDrainQueue,
                           _In_opt_ PVOID Cookie,
                           _Inout_ PVIRTIO_MSIX_WDM Msix)
{
    NTSTATUS status;
    ULONG messageCount;
    USHORT usedVectorCount;
    ULONG vector;
    IO_CONNECT_INTERRUPT_PARAMETERS params;

    if (Msix == NULL || InterruptDescTranslated == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (DeviceObject == NULL || PhysicalDeviceObject == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (InterruptDescTranslated->Type != CmResourceTypeInterrupt) {
        return STATUS_INVALID_PARAMETER;
    }

    if ((InterruptDescTranslated->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    if (QueueCount > 64) {
        return STATUS_NOT_SUPPORTED;
    }

    RtlZeroMemory(Msix, sizeof(*Msix));

    Msix->DeviceObject = DeviceObject;
    Msix->PhysicalDeviceObject = PhysicalDeviceObject;
    Msix->QueueCount = QueueCount;
    Msix->CommonCfgLock = CommonCfgLock;
    Msix->EvtConfigChange = EvtConfigChange;
    Msix->EvtDrainQueue = EvtDrainQueue;
    Msix->Cookie = Cookie;

    Msix->ConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    Msix->DpcInFlight = 0;

    messageCount = (ULONG)InterruptDescTranslated->u.MessageInterrupt.MessageCount;
    if (messageCount == 0) {
        RtlZeroMemory(Msix, sizeof(*Msix));
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    Msix->MessageCount = messageCount;

    usedVectorCount = 1;
    if (messageCount >= (1 + QueueCount)) {
        usedVectorCount = (USHORT)(1 + QueueCount);
    }
    Msix->UsedVectorCount = usedVectorCount;

    if (QueueCount != 0) {
        Msix->QueueLocks =
            (KSPIN_LOCK*)ExAllocatePoolWithTag(NonPagedPool, sizeof(KSPIN_LOCK) * QueueCount, VIRTIO_MSIX_WDM_POOL_TAG);
        if (Msix->QueueLocks == NULL) {
            status = STATUS_INSUFFICIENT_RESOURCES;
            goto fail;
        }

        Msix->QueueVectors =
            (USHORT*)ExAllocatePoolWithTag(NonPagedPool, sizeof(USHORT) * QueueCount, VIRTIO_MSIX_WDM_POOL_TAG);
        if (Msix->QueueVectors == NULL) {
            status = STATUS_INSUFFICIENT_RESOURCES;
            goto fail;
        }
        RtlZeroMemory(Msix->QueueVectors, sizeof(USHORT) * QueueCount);

        for (ULONG q = 0; q < QueueCount; q++) {
            KeInitializeSpinLock(&Msix->QueueLocks[q]);
        }
    }

    Msix->Vectors =
        (VIRTIO_MSIX_WDM_VECTOR*)ExAllocatePoolWithTag(NonPagedPool,
                                                      sizeof(VIRTIO_MSIX_WDM_VECTOR) * usedVectorCount,
                                                      VIRTIO_MSIX_WDM_POOL_TAG);
    if (Msix->Vectors == NULL) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto fail;
    }
    RtlZeroMemory(Msix->Vectors, sizeof(VIRTIO_MSIX_WDM_VECTOR) * usedVectorCount);

    for (vector = 0; vector < usedVectorCount; vector++) {
        ULONGLONG queueMask;
        BOOLEAN handlesConfig;

        queueMask = 0;
        handlesConfig = (vector == 0) ? TRUE : FALSE;

        if (usedVectorCount == 1) {
            queueMask = VirtioMsixQueueMaskAll(QueueCount);
            handlesConfig = TRUE;
        } else if (vector != 0) {
            queueMask = (1ULL << (vector - 1));
            handlesConfig = FALSE;
        }

        Msix->Vectors[vector].VectorIndex = (USHORT)vector;
        Msix->Vectors[vector].HandlesConfig = handlesConfig;
        Msix->Vectors[vector].QueueMask = queueMask;
        Msix->Vectors[vector].Msix = Msix;

        KeInitializeDpc(&Msix->Vectors[vector].Dpc, VirtioMsixDpc, &Msix->Vectors[vector]);
    }

    RtlZeroMemory(&params, sizeof(params));
    params.Version = CONNECT_MESSAGE_BASED;
    params.MessageBased.PhysicalDeviceObject = PhysicalDeviceObject;
    params.MessageBased.ServiceRoutine = VirtioMsixIsr;
    params.MessageBased.ServiceContext = Msix;
    params.MessageBased.SpinLock = NULL;
    params.MessageBased.SynchronizeIrql = (ULONG)InterruptDescTranslated->u.MessageInterrupt.Level;
    params.MessageBased.FloatingSave = FALSE;
    params.MessageBased.MessageCount = usedVectorCount;
    params.MessageBased.MessageInfo = NULL;
    params.MessageBased.ConnectionContext = NULL;

    status = IoConnectInterruptEx(&params);
    if (!NT_SUCCESS(status)) {
        goto fail;
    }

    Msix->MessageInfo = params.MessageBased.MessageInfo;
    Msix->ConnectionContext = params.MessageBased.ConnectionContext;

    /*
     * Derive the MSI-X table entry indices ("message numbers") that callers should
     * program into the virtio common_cfg routing fields (msix_config /
     * queue_msix_vector).
     *
     * IMPORTANT: Do NOT use MessageInfo[].MessageData here. MessageData is the
     * APIC vector encoded in the MSI/MSI-X message data value, which is not the
     * same thing as the MSI-X table entry index expected by virtio.
     *
     * IoConnectInterruptEx connects messages numbered 0..(MessageCount-1), and
     * passes that message number as MessageId to the ISR. Those message numbers
     * are the values that must be written into common_cfg.
     */
    Msix->ConfigVector = 0;

    if (Msix->QueueVectors != NULL) {
        if (usedVectorCount == 1) {
            for (ULONG q = 0; q < QueueCount; q++) {
                Msix->QueueVectors[q] = Msix->ConfigVector;
            }
        } else {
            for (ULONG q = 0; q < QueueCount; q++) {
                Msix->QueueVectors[q] = (USHORT)(1 + q);
            }
        }
    }

    Msix->Initialized = TRUE;
    return STATUS_SUCCESS;

fail:
    /*
     * Ensure teardown paths can safely call VirtioMsixDisconnect()
     * unconditionally even when connect failed mid-way.
     */
    VirtioMsixFreeAllocations(Msix);
    RtlZeroMemory(Msix, sizeof(*Msix));
    return status;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID VirtioMsixDisconnect(_Inout_ PVIRTIO_MSIX_WDM Msix)
{
    IO_DISCONNECT_INTERRUPT_PARAMETERS params;
    BOOLEAN removed;
    LONG remaining;
    LARGE_INTEGER delay;

    if (Msix == NULL) {
        return;
    }

    /*
     * Allow callers to unconditionally call VirtioMsixDisconnect() during PnP
     * teardown even when MSI/MSI-X was never connected (e.g. start failure).
     */
    if (!Msix->Initialized) {
        VirtioMsixFreeAllocations(Msix);
        RtlZeroMemory(Msix, sizeof(*Msix));
        return;
    }

    /* Ensure any late-running DPC does not call back into the driver. */
    Msix->EvtConfigChange = NULL;
    Msix->EvtDrainQueue = NULL;
    Msix->Cookie = NULL;

    if (Msix->ConnectionContext != NULL) {
        RtlZeroMemory(&params, sizeof(params));
        params.Version = DISCONNECT_MESSAGE_BASED;
        params.MessageBased.ConnectionContext = Msix->ConnectionContext;
        IoDisconnectInterruptEx(&params);
        Msix->ConnectionContext = NULL;
    }

    /* Cancel any DPCs that are queued but not yet running. */
    if (Msix->Vectors != NULL) {
        for (ULONG i = 0; i < Msix->UsedVectorCount; i++) {
            removed = KeRemoveQueueDpc(&Msix->Vectors[i].Dpc);
            if (removed) {
                remaining = InterlockedDecrement(&Msix->DpcInFlight);
                if (remaining < 0) {
                    (VOID)InterlockedExchange(&Msix->DpcInFlight, 0);
                }
            }
        }
    }

    /*
     * Wait for any in-flight DPC to finish before callers unmap MMIO/free queues.
     * (DpcInFlight tracks both queued and running DPC instances.)
     */
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        delay.QuadPart = -10 * 1000; /* 1ms */
        for (;;) {
            remaining = InterlockedCompareExchange(&Msix->DpcInFlight, 0, 0);
            if (remaining <= 0) {
                if (remaining < 0) {
                    (VOID)InterlockedExchange(&Msix->DpcInFlight, 0);
                }
                break;
            }

            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    } else {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        /*
         * Can't wait at elevated IRQL. Msix remains partially initialized so the
         * KDPCs stay valid if still running.
         */
        return;
    }

    Msix->MessageInfo = NULL;

    VirtioMsixFreeAllocations(Msix);
    RtlZeroMemory(Msix, sizeof(*Msix));
}

/*
 * PKMESSAGE_SERVICE_ROUTINE
 *
 * MSI/MSI-X does not require reading the virtio ISR status byte. The message ID
 * identifies which vector fired.
 */
static BOOLEAN VirtioMsixIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext, _In_ ULONG MessageId)
{
    PVIRTIO_MSIX_WDM msix;
    PVIRTIO_MSIX_WDM_VECTOR vec;
    BOOLEAN inserted;

    UNREFERENCED_PARAMETER(Interrupt);

    msix = (PVIRTIO_MSIX_WDM)ServiceContext;
    if (msix == NULL || msix->Vectors == NULL) {
        return FALSE;
    }

    if (MessageId >= (ULONG)msix->UsedVectorCount) {
        return FALSE;
    }

    vec = &msix->Vectors[MessageId];

    /* Track queued + running DPC instances (across all vectors). */
    (VOID)InterlockedIncrement(&msix->DpcInFlight);
    inserted = KeInsertQueueDpc(&vec->Dpc, NULL, NULL);
    if (!inserted) {
        LONG rem = InterlockedDecrement(&msix->DpcInFlight);
        if (rem < 0) {
            (VOID)InterlockedExchange(&msix->DpcInFlight, 0);
        }
    }

    return TRUE;
}

/*
 * PKDEFERRED_ROUTINE
 *
 * Runs at DISPATCH_LEVEL.
 */
static VOID VirtioMsixDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2)
{
    PVIRTIO_MSIX_WDM_VECTOR vec;
    PVIRTIO_MSIX_WDM msix;
    LONG remaining;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    vec = (PVIRTIO_MSIX_WDM_VECTOR)DeferredContext;
    if (vec == NULL) {
        return;
    }

    msix = vec->Msix;
    if (msix == NULL) {
        return;
    }

    if (vec->HandlesConfig && (msix->EvtConfigChange != NULL)) {
        msix->EvtConfigChange(msix->DeviceObject, msix->Cookie);
    }

    if ((msix->EvtDrainQueue != NULL) && (vec->QueueMask != 0)) {
        for (ULONG q = 0; q < msix->QueueCount; q++) {
            KIRQL oldIrql;

            if ((vec->QueueMask & (1ULL << q)) == 0) {
                continue;
            }

            if (msix->QueueLocks != NULL) {
                KeAcquireSpinLock(&msix->QueueLocks[q], &oldIrql);
            } else {
                oldIrql = DISPATCH_LEVEL;
            }

            msix->EvtDrainQueue(msix->DeviceObject, q, msix->Cookie);

            if (msix->QueueLocks != NULL) {
                KeReleaseSpinLock(&msix->QueueLocks[q], oldIrql);
            }
        }
    }

    remaining = InterlockedDecrement(&msix->DpcInFlight);
    if (remaining < 0) {
        (VOID)InterlockedExchange(&msix->DpcInFlight, 0);
    }
}
