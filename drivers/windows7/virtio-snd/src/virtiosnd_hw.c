/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "pci_interface.h"
#include "virtiosnd.h"
#include "virtiosnd_contract.h"
#include "virtiosnd_intx.h"

/* Bounded reset poll (virtio status reset handshake). */
#define VIRTIOSND_RESET_TIMEOUT_US 1000000u
#define VIRTIOSND_RESET_POLL_DELAY_US 1000u
#define VIRTIOSND_RESET_HIGH_IRQL_TIMEOUT_US 10000u
#define VIRTIOSND_RESET_HIGH_IRQL_POLL_DELAY_US 100u

static __forceinline UCHAR VirtIoSndReadDeviceStatus(_In_ const VIRTIO_PCI_MODERN_TRANSPORT *Transport)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->device_status);
}

static __forceinline VOID VirtIoSndWriteDeviceStatus(_In_ const VIRTIO_PCI_MODERN_TRANSPORT *Transport, _In_ UCHAR Status)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)&Transport->CommonCfg->device_status, Status);
}

static VOID VirtIoSndResetDeviceBestEffort(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    KIRQL irql;
    ULONG waitedUs;
    UCHAR status;

    if (Dx == NULL || Dx->Transport.CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    VirtIoSndWriteDeviceStatus(&Dx->Transport, 0);
    KeMemoryBarrier();

    /* Immediate readback fast-path. */
    status = VirtIoSndReadDeviceStatus(&Dx->Transport);
    if (status == 0) {
        KeMemoryBarrier();
        return;
    }

    irql = KeGetCurrentIrql();
    if (irql == PASSIVE_LEVEL) {
        const ULONGLONG timeout100ns = (ULONGLONG)VIRTIOSND_RESET_TIMEOUT_US * 10ull;
        const ULONGLONG pollDelay100ns = (ULONGLONG)VIRTIOSND_RESET_POLL_DELAY_US * 10ull;
        const ULONGLONG start100ns = KeQueryInterruptTime();
        const ULONGLONG deadline100ns = start100ns + timeout100ns;

        for (;;) {
            ULONGLONG now100ns;
            ULONGLONG remaining100ns;
            LARGE_INTEGER delay;

            status = VirtIoSndReadDeviceStatus(&Dx->Transport);
            if (status == 0) {
                KeMemoryBarrier();
                return;
            }

            now100ns = KeQueryInterruptTime();
            if (now100ns >= deadline100ns) {
                break;
            }

            remaining100ns = deadline100ns - now100ns;
            if (remaining100ns > pollDelay100ns) {
                remaining100ns = pollDelay100ns;
            }

            delay.QuadPart = -((LONGLONG)remaining100ns);
            (void)KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }

        VIRTIOSND_TRACE_ERROR(
            "reset: device_status did not clear within %lu us (IRQL=%lu), last=%lu\n",
            (ULONG)VIRTIOSND_RESET_TIMEOUT_US,
            (ULONG)irql,
            (ULONG)status);
        return;
    }

    for (waitedUs = 0; waitedUs < VIRTIOSND_RESET_HIGH_IRQL_TIMEOUT_US;
         waitedUs += VIRTIOSND_RESET_HIGH_IRQL_POLL_DELAY_US) {
        KeStallExecutionProcessor(VIRTIOSND_RESET_HIGH_IRQL_POLL_DELAY_US);

        status = VirtIoSndReadDeviceStatus(&Dx->Transport);
        if (status == 0) {
            KeMemoryBarrier();
            return;
        }
    }

    VIRTIOSND_TRACE_ERROR(
        "reset: device_status did not clear within %lu us at IRQL=%lu, last=%lu\n",
        (ULONG)VIRTIOSND_RESET_HIGH_IRQL_TIMEOUT_US,
        (ULONG)irql,
        (ULONG)status);
}

static VOID VirtIoSndFailDeviceBestEffort(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    VirtioPciModernTransportAddStatus(&Dx->Transport, VIRTIO_STATUS_FAILED);
}

static UINT8 VirtIoSndTransportPciRead8(void *context, UINT16 offset)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)context;

    if (dx == NULL || offset >= (UINT16)sizeof(dx->PciCfgSpace)) {
        return 0;
    }

    return (UINT8)dx->PciCfgSpace[offset];
}

static UINT16 VirtIoSndTransportPciRead16(void *context, UINT16 offset)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)context;

    if (dx == NULL || (UINT32)offset + sizeof(UINT16) > sizeof(dx->PciCfgSpace)) {
        return 0;
    }

    return (UINT16)dx->PciCfgSpace[offset] | ((UINT16)dx->PciCfgSpace[offset + 1] << 8);
}

static UINT32 VirtIoSndTransportPciRead32(void *context, UINT16 offset)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = (PVIRTIOSND_DEVICE_EXTENSION)context;

    if (dx == NULL || (UINT32)offset + sizeof(UINT32) > sizeof(dx->PciCfgSpace)) {
        return 0;
    }

    return (UINT32)dx->PciCfgSpace[offset] | ((UINT32)dx->PciCfgSpace[offset + 1] << 8) |
           ((UINT32)dx->PciCfgSpace[offset + 2] << 16) | ((UINT32)dx->PciCfgSpace[offset + 3] << 24);
}

static NTSTATUS VirtIoSndTransportMapMmio(void *context, UINT64 physicalAddress, UINT32 length, volatile void **mappedVaOut)
{
    PHYSICAL_ADDRESS pa;
    PVOID va;

    UNREFERENCED_PARAMETER(context);

    if (mappedVaOut != NULL) {
        *mappedVaOut = NULL;
    }

    if (mappedVaOut == NULL || physicalAddress == 0 || length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    pa.QuadPart = (LONGLONG)physicalAddress;
    va = MmMapIoSpace(pa, (SIZE_T)length, MmNonCached);
    if (va == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    *mappedVaOut = (volatile void *)va;
    return STATUS_SUCCESS;
}

static void VirtIoSndTransportUnmapMmio(void *context, volatile void *mappedVa, UINT32 length)
{
    UNREFERENCED_PARAMETER(context);

    if (mappedVa != NULL && length != 0) {
        MmUnmapIoSpace((PVOID)mappedVa, (SIZE_T)length);
    }
}

static void VirtIoSndTransportStallUs(void *context, UINT32 microseconds)
{
    UNREFERENCED_PARAMETER(context);
    KeStallExecutionProcessor(microseconds);
}

static void VirtIoSndTransportMemoryBarrier(void *context)
{
    UNREFERENCED_PARAMETER(context);
    KeMemoryBarrier();
}

static void *VirtIoSndTransportSpinlockCreate(void *context)
{
    UNREFERENCED_PARAMETER(context);

    {
        KSPIN_LOCK *lock = (KSPIN_LOCK *)ExAllocatePoolWithTag(NonPagedPool, sizeof(KSPIN_LOCK), VIRTIOSND_POOL_TAG);
        if (lock == NULL) {
            return NULL;
        }
        KeInitializeSpinLock(lock);
        return lock;
    }
}

static void VirtIoSndTransportSpinlockDestroy(void *context, void *lock)
{
    UNREFERENCED_PARAMETER(context);
    if (lock != NULL) {
        ExFreePoolWithTag(lock, VIRTIOSND_POOL_TAG);
    }
}

static void VirtIoSndTransportSpinlockAcquire(void *context, void *lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE *stateOut)
{
    KIRQL oldIrql;

    UNREFERENCED_PARAMETER(context);

    if (stateOut != NULL) {
        *stateOut = 0;
    }

    if (lock == NULL || stateOut == NULL) {
        return;
    }

    KeAcquireSpinLock((PKSPIN_LOCK)lock, &oldIrql);
    *stateOut = (VIRTIO_PCI_MODERN_SPINLOCK_STATE)oldIrql;
}

static void VirtIoSndTransportSpinlockRelease(void *context, void *lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE state)
{
    UNREFERENCED_PARAMETER(context);

    if (lock == NULL) {
        return;
    }

    KeReleaseSpinLock((PKSPIN_LOCK)lock, (KIRQL)state);
}

static void VirtIoSndTransportLog(void *context, const char *message)
{
    UNREFERENCED_PARAMETER(context);

    if (message != NULL) {
        VIRTIOSND_TRACE("%s\n", message);
    }
}

static ULONG VirtIoSndReadLe32FromCfg(_In_reads_bytes_(256) const UCHAR *cfg, _In_ ULONG offset)
{
    ULONG v;

    v = 0;
    if (cfg == NULL || offset + sizeof(v) > 256u) {
        return 0;
    }

    RtlCopyMemory(&v, cfg + offset, sizeof(v));
    return v;
}

static ULONGLONG VirtIoSndComputeBar0Base(_In_reads_bytes_(256) const UCHAR *cfg)
{
    ULONG bar0Low = VirtIoSndReadLe32FromCfg(cfg, 0x10u);
    ULONG bar0High = 0;
    ULONG memType;
    ULONGLONG base;

    if (bar0Low == 0) {
        return 0;
    }

    if ((bar0Low & 0x1u) != 0) {
        /* I/O BAR (unsupported by contract). */
        return (ULONGLONG)(bar0Low & ~0x3u);
    }

    memType = (bar0Low >> 1) & 0x3u;
    base = (ULONGLONG)(bar0Low & ~0xFu);
    if (memType == 0x2u) {
        bar0High = VirtIoSndReadLe32FromCfg(cfg, 0x14u);
        base |= ((ULONGLONG)bar0High << 32);
    }

    return base;
}

static BOOLEAN VirtIoSndExtractMemoryResource(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR *desc,
                                             _Out_ PHYSICAL_ADDRESS *startOut,
                                             _Out_ ULONGLONG *lengthBytesOut)
{
    USHORT large;

    if (startOut != NULL) {
        startOut->QuadPart = 0;
    }
    if (lengthBytesOut != NULL) {
        *lengthBytesOut = 0;
    }

    if (desc == NULL || startOut == NULL || lengthBytesOut == NULL) {
        return FALSE;
    }

    if (desc->Type == CmResourceTypeMemory) {
        *startOut = desc->u.Memory.Start;
        *lengthBytesOut = (ULONGLONG)desc->u.Memory.Length;
        return TRUE;
    }

    if (desc->Type == CmResourceTypeMemoryLarge) {
        /*
         * CmResourceTypeMemoryLarge encodes length in scaled units. Decode it
         * back to bytes per WDK definitions.
         */
        large = desc->Flags & (CM_RESOURCE_MEMORY_LARGE_40 | CM_RESOURCE_MEMORY_LARGE_48 | CM_RESOURCE_MEMORY_LARGE_64);
        switch (large) {
            case CM_RESOURCE_MEMORY_LARGE_40:
                *startOut = desc->u.Memory40.Start;
                *lengthBytesOut = ((ULONGLONG)desc->u.Memory40.Length40) << 8; /* 256B units */
                return TRUE;
            case CM_RESOURCE_MEMORY_LARGE_48:
                *startOut = desc->u.Memory48.Start;
                *lengthBytesOut = ((ULONGLONG)desc->u.Memory48.Length48) << 16; /* 64KiB units */
                return TRUE;
            case CM_RESOURCE_MEMORY_LARGE_64:
                *startOut = desc->u.Memory64.Start;
                *lengthBytesOut = ((ULONGLONG)desc->u.Memory64.Length64) << 32; /* 4GiB units */
                return TRUE;
            default:
                return FALSE;
        }
    }

    return FALSE;
}

static NTSTATUS VirtIoSndFindBar0Resource(_In_ ULONGLONG bar0Base,
                                         _In_ PCM_RESOURCE_LIST resourcesRaw,
                                         _In_ PCM_RESOURCE_LIST resourcesTranslated,
                                         _Out_ PHYSICAL_ADDRESS *translatedStartOut,
                                         _Out_ UINT32 *lengthOut)
{
    ULONG fullIndex;
    ULONG fullCount;

    if (translatedStartOut != NULL) {
        translatedStartOut->QuadPart = 0;
    }
    if (lengthOut != NULL) {
        *lengthOut = 0;
    }

    if (bar0Base == 0 || resourcesRaw == NULL || resourcesTranslated == NULL || translatedStartOut == NULL || lengthOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    fullCount = resourcesRaw->Count;
    if (resourcesTranslated->Count < fullCount) {
        fullCount = resourcesTranslated->Count;
    }

    for (fullIndex = 0; fullIndex < fullCount; ++fullIndex) {
        PCM_FULL_RESOURCE_DESCRIPTOR rawFull;
        PCM_FULL_RESOURCE_DESCRIPTOR transFull;
        PCM_PARTIAL_RESOURCE_LIST rawList;
        PCM_PARTIAL_RESOURCE_LIST transList;
        ULONG descCount;
        ULONG descIndex;

        rawFull = &resourcesRaw->List[fullIndex];
        transFull = &resourcesTranslated->List[fullIndex];

        rawList = &rawFull->PartialResourceList;
        transList = &transFull->PartialResourceList;

        descCount = rawList->Count;
        if (transList->Count < descCount) {
            descCount = transList->Count;
        }

        for (descIndex = 0; descIndex < descCount; ++descIndex) {
            PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
            PCM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
            ULONGLONG rawStart;
            ULONG len;

            rawDesc = &rawList->PartialDescriptors[descIndex];
            transDesc = &transList->PartialDescriptors[descIndex];

            {
                PHYSICAL_ADDRESS rawStartPa;
                PHYSICAL_ADDRESS transStartPa;
                ULONGLONG rawLenBytes;
                ULONGLONG transLenBytes;

                if (!VirtIoSndExtractMemoryResource(rawDesc, &rawStartPa, &rawLenBytes) ||
                    !VirtIoSndExtractMemoryResource(transDesc, &transStartPa, &transLenBytes)) {
                    continue;
                }

                rawStart = (ULONGLONG)rawStartPa.QuadPart;
                if (rawStart != bar0Base) {
                    continue;
                }

                UNREFERENCED_PARAMETER(transLenBytes);

                if (rawLenBytes == 0) {
                    return STATUS_DEVICE_CONFIGURATION_ERROR;
                }
                if (rawLenBytes > 0xFFFFFFFFull || transLenBytes > 0xFFFFFFFFull) {
                    return STATUS_DEVICE_CONFIGURATION_ERROR;
                }

                if (transLenBytes < rawLenBytes) {
                    rawLenBytes = transLenBytes;
                }

                len = (ULONG)rawLenBytes;
                *translatedStartOut = transStartPa;
                *lengthOut = (UINT32)len;
            return STATUS_SUCCESS;
        }
            }
    }

    return STATUS_DEVICE_CONFIGURATION_ERROR;
}

static VOID VirtIoSndDestroyQueues(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    ULONG i;
    for (i = 0; i < VIRTIOSND_QUEUE_COUNT; ++i) {
        VirtioSndQueueSplitDestroy(&Dx->DmaCtx, &Dx->QueueSplit[i]);
        Dx->Queues[i].Ops = NULL;
        Dx->Queues[i].Ctx = NULL;
    }
}

static __forceinline ULONG VirtIoSndReadLe32(_In_reads_bytes_(4) const UCHAR *p)
{
    if (p == NULL) {
        return 0;
    }
    return (ULONG)p[0] | ((ULONG)p[1] << 8) | ((ULONG)p[2] << 16) | ((ULONG)p[3] << 24);
}

static NTSTATUS VirtIoSndValidateDeviceCfg(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;
    UCHAR cfg[0x0Cu];
    ULONG jacks;
    ULONG streams;
    ULONG chmaps;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Transport.DeviceCfg == NULL || Dx->Transport.DeviceCfgLength < 0x0Cu) {
        VIRTIOSND_TRACE_ERROR(
            "virtio-snd DEVICE_CFG unavailable/too small: DeviceCfg=%p len=0x%lx (need >= 0x0C)\n",
            Dx->Transport.DeviceCfg,
            (ULONG)Dx->Transport.DeviceCfgLength);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    RtlZeroMemory(cfg, sizeof(cfg));
    status = VirtioPciModernTransportReadDeviceConfig(&Dx->Transport, /*Offset=*/0, cfg, (UINT32)sizeof(cfg));
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to read virtio-snd DEVICE_CFG: 0x%08X\n", (UINT)status);
        return status;
    }

    jacks = VirtIoSndReadLe32(cfg + 0x00u);
    streams = VirtIoSndReadLe32(cfg + 0x04u);
    chmaps = VirtIoSndReadLe32(cfg + 0x08u);

    if (!VirtIoSndValidateDeviceCfgValues(jacks, streams, chmaps)) {
        VIRTIOSND_TRACE_ERROR(
            "virtio-snd DEVICE_CFG violates contract v1: jacks=%lu streams=%lu chmaps=%lu (expected 0/2/0)\n",
            jacks,
            streams,
            chmaps);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    return STATUS_SUCCESS;
}

static VOID VirtIoSndEventqUninit(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    Dx->EventqBufferCount = 0;
    VirtIoSndFreeCommonBuffer(&Dx->DmaCtx, &Dx->EventqBufferPool);
    Dx->EventqBufferCount = 0;
}

/*
 * Best-effort: virtio-snd contract v1 does not define event messages, so failure
 * to allocate/post eventq buffers must not prevent audio streaming.
 */
static VOID VirtIoSndEventqInitBestEffort(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;
    ULONG desired;
    ULONG posted;
    USHORT qsz;
    SIZE_T totalBytes;
    ULONG i;
    VIRTIOSND_QUEUE* q;

    if (Dx == NULL) {
        return;
    }

    VirtIoSndEventqUninit(Dx);

    q = &Dx->Queues[VIRTIOSND_QUEUE_EVENT];
    if (q->Ops == NULL || q->Ctx == NULL) {
        return;
    }

    desired = VIRTIOSND_EVENTQ_BUFFER_COUNT;
    qsz = VirtioSndQueueGetSize(q);
    if (qsz != 0 && desired > (ULONG)qsz) {
        desired = (ULONG)qsz;
    }

    if (desired == 0) {
        return;
    }

    totalBytes = (SIZE_T)desired * (SIZE_T)VIRTIOSND_EVENTQ_BUFFER_SIZE;
    status = VirtIoSndAllocCommonBuffer(&Dx->DmaCtx, totalBytes, FALSE, &Dx->EventqBufferPool);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("eventq: failed to allocate buffer pool (%Iu bytes): 0x%08X\n", totalBytes, (UINT)status);
        VirtIoSndEventqUninit(Dx);
        return;
    }

    /*
     * This DMA buffer is shared with the (potentially untrusted) device; clear
     * it to avoid leaking stale kernel memory.
     */
    RtlZeroMemory(Dx->EventqBufferPool.Va, Dx->EventqBufferPool.Size);
    Dx->EventqBufferCount = desired;

    posted = 0;
    for (i = 0; i < desired; ++i) {
        VIRTIOSND_SG sg;
        PUCHAR va;
        UINT64 dma;

        va = (PUCHAR)Dx->EventqBufferPool.Va + ((SIZE_T)i * (SIZE_T)VIRTIOSND_EVENTQ_BUFFER_SIZE);
        dma = Dx->EventqBufferPool.DmaAddr + ((UINT64)i * (UINT64)VIRTIOSND_EVENTQ_BUFFER_SIZE);

        sg.addr = dma;
        sg.len = (UINT32)VIRTIOSND_EVENTQ_BUFFER_SIZE;
        sg.write = TRUE; /* device writes event messages */

        status = VirtioSndQueueSubmit(q, &sg, 1, va);
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("eventq: failed to post buffer %lu/%lu: 0x%08X\n", i, desired, (UINT)status);
            break;
        }
        posted++;
    }

    if (posted == 0) {
        VirtIoSndEventqUninit(Dx);
        return;
    }

    VirtioSndQueueKick(q);
}

static NTSTATUS VirtIoSndSetupQueues(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;
    ULONG q;
    const BOOLEAN eventIdx = (Dx->NegotiatedFeatures & (UINT64)VIRTIO_RING_F_EVENT_IDX) != 0;
    const BOOLEAN indirect = (Dx->NegotiatedFeatures & (UINT64)VIRTIO_RING_F_INDIRECT_DESC) != 0;

    /*
     * Contract v1 requires four virtqueues (control/event/tx/rx).
     */
    if (Dx->Transport.CommonCfg != NULL) {
        USHORT numQueues;

        numQueues = READ_REGISTER_USHORT((volatile USHORT *)&Dx->Transport.CommonCfg->num_queues);
        if (numQueues < (USHORT)VIRTIOSND_QUEUE_COUNT) {
            VIRTIOSND_TRACE_ERROR(
                "device exposes %u queues (< %u required by contract v1)\n",
                (UINT)numQueues,
                (UINT)VIRTIOSND_QUEUE_COUNT);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
        USHORT size;
        USHORT expectedSize;
        USHORT notifyOff;
        volatile UINT16* notifyAddr;
        UINT64 descPa, availPa, usedPa;
        USHORT notifyOffReadback;
        notifyOff = 0;
        notifyAddr = NULL;
        descPa = 0;
        availPa = 0;
        usedPa = 0;

        expectedSize = VirtIoSndExpectedQueueSize((USHORT)q);
        if (expectedSize == 0) {
            VIRTIOSND_TRACE_ERROR("queue %lu has no contract-v1 expected size mapping\n", q);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        status = VirtioPciModernTransportGetQueueSize(&Dx->Transport, (USHORT)q, &size);
        if (!NT_SUCCESS(status)) {
            if (status == STATUS_NOT_FOUND || size == 0) {
                VIRTIOSND_TRACE_ERROR(
                    "queue %lu reports size=0 but contract v1 requires size=%u\n",
                    q,
                    (UINT)expectedSize);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }
            return status;
        }

        if (size != expectedSize) {
            VIRTIOSND_TRACE_ERROR(
                "queue %lu reports size=%u but contract v1 requires size=%u\n",
                q,
                (UINT)size,
                (UINT)expectedSize);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if ((size & (size - 1u)) != 0) {
            VIRTIOSND_TRACE_ERROR(
                "queue %lu reports non-power-of-two size=%u\n",
                q,
                (UINT)size);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        status = VirtioPciModernTransportGetQueueNotifyOff(&Dx->Transport, (USHORT)q, &notifyOff);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        if (Dx->Transport.NotifyBase == NULL || Dx->Transport.NotifyOffMultiplier == 0) {
            return STATUS_INVALID_DEVICE_STATE;
        }

        {
            const UINT64 notifyByteOff = (UINT64)notifyOff * (UINT64)Dx->Transport.NotifyOffMultiplier;
            if (notifyByteOff + sizeof(UINT16) > (UINT64)Dx->Transport.NotifyLength) {
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }
            notifyAddr = (volatile UINT16 *)(Dx->Transport.NotifyBase + (UINT32)notifyByteOff);
        }

        status = VirtioSndQueueSplitCreate(
            &Dx->DmaCtx,
            &Dx->QueueSplit[q],
            (USHORT)q,
            size,
            eventIdx,
            indirect,
            notifyAddr,
            &Dx->Queues[q],
            &descPa,
            &availPa,
            &usedPa);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        /*
         * Disable MSI-X for this queue; INTx/ISR is required by contract v1.
         *
         * Use a best-effort call here: if the device/transport rejects MSI-X
         * programming (read-back mismatch) we still proceed with INTx.
         */
        (void)VirtioPciModernTransportSetQueueMsixVector(&Dx->Transport, (USHORT)q, VIRTIO_PCI_MSI_NO_VECTOR);

        status = VirtioPciModernTransportSetupQueue(&Dx->Transport, (USHORT)q, descPa, availPa, usedPa);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        notifyOffReadback = 0;
        status = VirtioPciModernTransportGetQueueNotifyOff(&Dx->Transport, (USHORT)q, &notifyOffReadback);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        if (notifyOffReadback != notifyOff) {
            VIRTIOSND_TRACE_ERROR(
                "queue %lu notify_off readback mismatch: init=%u readback=%u\n",
                q,
                (UINT)notifyOff,
                (UINT)notifyOffReadback);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        VIRTIOSND_TRACE("queue %lu enabled (size=%u)\n", q, (UINT)size);

        if (Dx->QueueSplit[q].Ring.Va != NULL) {
            VIRTIOSND_TRACE(
                "queue %lu ring: VA=%p DMA=%I64x bytes=%Iu\n",
                q,
                Dx->QueueSplit[q].Ring.Va,
                (ULONGLONG)Dx->QueueSplit[q].Ring.DmaAddr,
                Dx->QueueSplit[q].Ring.Size);

            VIRTIOSND_TRACE(
                "queue %lu desc VA=%p PA=%I64x | avail VA=%p PA=%I64x | used VA=%p PA=%I64x\n",
                q,
                Dx->QueueSplit[q].Vq->desc,
                (ULONGLONG)descPa,
                Dx->QueueSplit[q].Vq->avail,
                (ULONGLONG)availPa,
                Dx->QueueSplit[q].Vq->used,
                (ULONGLONG)usedPa);
        }
    }

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID VirtIoSndStopHardware(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS cancelStatus;

    if (Dx == NULL) {
        return;
    }

    /*
     * Stop accepting new TX/control submissions as early as possible. WaveRT's
     * period timer runs independently of the virtio interrupt DPC; dropping this
     * flag up-front prevents racey writes while teardown is in progress.
     */
    Dx->Started = FALSE;

    cancelStatus = Dx->Removed ? STATUS_DEVICE_REMOVED : STATUS_CANCELLED;

    VirtIoSndIntxDisconnect(Dx);

    /*
     * On SURPRISE_REMOVAL the device may already be gone from the PCI bus. Avoid
     * MMIO accesses (device_status reset handshake) in that case.
     */
    if (!Dx->Removed) {
        VirtIoSndResetDeviceBestEffort(Dx);
    }

    /*
     * Cancel and drain protocol operations before teardown so request DMA common
     * buffers are freed while the DMA adapter is still valid.
     *
     * Note: StopHardware is also used as a best-effort cleanup routine on the
     * first START_DEVICE before the control engine has been initialized. Guard
     * against calling Control::Uninit on a zeroed (uninitialized) struct.
     */
    if (Dx->Control.DmaCtx != NULL) {
        /*
         * Drain any already-completed used entries before canceling requests.
         * This avoids racing with the send thread freeing request cookies while
         * they may still be present in the virtqueue used ring.
         */
        VirtioSndCtrlProcessUsed(&Dx->Control);

        VirtioSndCtrlCancelAll(&Dx->Control, cancelStatus);
        VirtioSndCtrlUninit(&Dx->Control);
    }

    VirtioSndTxUninit(&Dx->Tx);
    (VOID)InterlockedExchange(&Dx->TxEngineInitialized, 0);

    (VOID)InterlockedExchange(&Dx->RxEngineInitialized, 0);
    VirtIoSndRxUninit(&Dx->Rx);

    VirtIoSndEventqUninit(Dx);

    VirtIoSndDestroyQueues(Dx);

    VirtIoSndDmaUninit(&Dx->DmaCtx);

    VirtioPciModernTransportUninit(&Dx->Transport);

    VirtIoSndReleaseBusInterface(&Dx->PciInterface, &Dx->PciInterfaceAcquired);
    RtlZeroMemory(Dx->PciCfgSpace, sizeof(Dx->PciCfgSpace));
    RtlZeroMemory(&Dx->TransportOs, sizeof(Dx->TransportOs));

    Dx->NegotiatedFeatures = 0;
}

_Use_decl_annotations_
VOID VirtIoSndHwResetDeviceForTeardown(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return;
    }

    VIRTIOSND_TRACE_ERROR("hw: emergency reset requested (Started=%u Removed=%u)\n", Dx->Started ? 1u : 0u, Dx->Removed ? 1u : 0u);

    /*
     * Stop accepting new submissions immediately so periodic WaveRT timers don't
     * race with teardown.
     */
    Dx->Started = FALSE;

    /*
     * Ensure any later queue draining (e.g. in STOP_DEVICE / REMOVE_DEVICE
     * teardown) cannot invoke a stale capture completion callback with a freed
     * stream pointer.
     */
    if (InterlockedCompareExchange(&Dx->RxEngineInitialized, 0, 0) != 0 && Dx->Rx.Queue != NULL && Dx->Rx.Requests != NULL) {
        VirtIoSndRxSetCompletionCallback(&Dx->Rx, NULL, NULL);
    }

    /*
     * Disconnect INTx and wait for any in-flight DPC to complete. This prevents
     * further completion delivery while higher layers free their DMA buffers.
     */
    VirtIoSndIntxDisconnect(Dx);

    /*
     * On SURPRISE_REMOVAL the device may already be gone from the PCI bus. Avoid
     * MMIO accesses (device_status reset handshake) in that case.
     */
    if (!Dx->Removed) {
        VirtIoSndResetDeviceBestEffort(Dx);
    }
}

_Use_decl_annotations_
NTSTATUS VirtIoSndStartHardware(
    PVIRTIOSND_DEVICE_EXTENSION Dx,
    PCM_RESOURCE_LIST RawResources,
    PCM_RESOURCE_LIST TranslatedResources)
{
    NTSTATUS status;
    UINT32 bar0Length;
    PHYSICAL_ADDRESS bar0TranslatedStart;
    ULONGLONG bar0Base;
    ULONG cfgBytes;
    VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR initErr;
    const UINT64 requiredFeatures = VIRTIO_F_VERSION_1 | (UINT64)VIRTIO_RING_F_INDIRECT_DESC;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    VirtIoSndStopHardware(Dx);

    status = VirtIoSndAcquireBusInterface(Dx->LowerDeviceObject, &Dx->PciInterface, &Dx->PciInterfaceAcquired);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndAcquireBusInterface failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    cfgBytes = VirtIoSndBusReadConfig(&Dx->PciInterface, Dx->PciCfgSpace, 0, (ULONG)sizeof(Dx->PciCfgSpace));
    if (cfgBytes != sizeof(Dx->PciCfgSpace)) {
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        VIRTIOSND_TRACE_ERROR("failed to read PCI config space (got %lu)\n", cfgBytes);
        goto fail;
    }

    bar0Base = VirtIoSndComputeBar0Base(Dx->PciCfgSpace);
    bar0Length = 0;
    bar0TranslatedStart.QuadPart = 0;
    status = VirtIoSndFindBar0Resource(bar0Base, RawResources, TranslatedResources, &bar0TranslatedStart, &bar0Length);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to locate BAR0 resource: 0x%08X\n", (UINT)status);
        goto fail;
    }

    RtlZeroMemory(&Dx->TransportOs, sizeof(Dx->TransportOs));
    Dx->TransportOs.Context = Dx;
    Dx->TransportOs.PciRead8 = VirtIoSndTransportPciRead8;
    Dx->TransportOs.PciRead16 = VirtIoSndTransportPciRead16;
    Dx->TransportOs.PciRead32 = VirtIoSndTransportPciRead32;
    Dx->TransportOs.MapMmio = VirtIoSndTransportMapMmio;
    Dx->TransportOs.UnmapMmio = VirtIoSndTransportUnmapMmio;
    Dx->TransportOs.StallUs = VirtIoSndTransportStallUs;
    Dx->TransportOs.MemoryBarrier = VirtIoSndTransportMemoryBarrier;
    Dx->TransportOs.SpinlockCreate = VirtIoSndTransportSpinlockCreate;
    Dx->TransportOs.SpinlockDestroy = VirtIoSndTransportSpinlockDestroy;
    Dx->TransportOs.SpinlockAcquire = VirtIoSndTransportSpinlockAcquire;
    Dx->TransportOs.SpinlockRelease = VirtIoSndTransportSpinlockRelease;
    Dx->TransportOs.Log = VirtIoSndTransportLog;

    status = VirtioPciModernTransportInit(
        &Dx->Transport,
        &Dx->TransportOs,
        VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT,
        (UINT64)bar0TranslatedStart.QuadPart,
        bar0Length);
    if (!NT_SUCCESS(status)) {
        initErr = Dx->Transport.InitError;
        if (initErr == VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH ||
            initErr == VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_64BIT_MMIO ||
            initErr == VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL) {
            VirtioPciModernTransportUninit(&Dx->Transport);
            status = VirtioPciModernTransportInit(
                &Dx->Transport,
                &Dx->TransportOs,
                VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT,
                (UINT64)bar0TranslatedStart.QuadPart,
                bar0Length);
        }
    }

    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR(
            "transport init failed: %s (0x%08X)\n",
            VirtioPciModernTransportInitErrorStr(Dx->Transport.InitError),
            (UINT)status);
        goto fail;
    }

    VIRTIOSND_TRACE(
        "transport: mode=%s rev=0x%02X bar0_pa=0x%I64x len=0x%I64x notify_mult=%lu\n",
        (Dx->Transport.Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT) ? "strict" : "compat",
        (UINT)Dx->Transport.PciRevisionId,
        (ULONGLONG)Dx->Transport.Bar0Pa,
        (ULONGLONG)Dx->Transport.Bar0Length,
        (ULONG)Dx->Transport.NotifyOffMultiplier);

    /*
     * Contract v1 requires VIRTIO_RING_F_INDIRECT_DESC and uses split virtqueues.
     * EVENT_IDX/PACKED are tolerated but are not negotiated by this driver.
     */
    status = VirtioPciModernTransportNegotiateFeatures(
        &Dx->Transport,
        /*Required=*/requiredFeatures,
        /*Wanted=*/0,
        &Dx->NegotiatedFeatures);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("feature negotiation failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    /*
     * Disable MSI-X config interrupt vector; INTx/ISR is required by contract v1.
     *
     * Best-effort: any failure here should not prevent INTx bring-up.
     */
    (void)VirtioPciModernTransportSetConfigMsixVector(&Dx->Transport, VIRTIO_PCI_MSI_NO_VECTOR);

    VIRTIOSND_TRACE("features negotiated: 0x%I64x\n", Dx->NegotiatedFeatures);

    status = VirtIoSndValidateDeviceCfg(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("virtio-snd DEVICE_CFG validation failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    status = VirtIoSndDmaInit(Dx->Pdo, &Dx->DmaCtx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndDmaInit failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    status = VirtIoSndIntxCaptureResources(Dx, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to locate INTx resource: 0x%08X\n", (UINT)status);
        goto fail;
    }

    status = VirtIoSndSetupQueues(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("queue setup failed: 0x%08X\n", (UINT)status);
        goto fail;
    }

    VirtIoSndEventqInitBestEffort(Dx);

    /* Initialize the protocol engines now that queues are available. */
    VirtioSndCtrlInit(&Dx->Control, &Dx->DmaCtx, &Dx->Queues[VIRTIOSND_QUEUE_CONTROL]);

    RtlZeroMemory(&Dx->Tx, sizeof(Dx->Tx));
    Dx->TxEngineInitialized = 0;
    RtlZeroMemory(&Dx->Rx, sizeof(Dx->Rx));
    Dx->RxEngineInitialized = 0;

    status = VirtIoSndIntxConnect(Dx);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to connect INTx: 0x%08X\n", (UINT)status);
        goto fail;
    }

    /* The device is now ready for normal operation. */
    VirtioPciModernTransportAddStatus(&Dx->Transport, VIRTIO_STATUS_DRIVER_OK);

    VIRTIOSND_TRACE("device_status=0x%02X\n", (UINT)VirtIoSndReadDeviceStatus(&Dx->Transport));

    Dx->Started = TRUE;
    return STATUS_SUCCESS;

fail:
    VirtIoSndFailDeviceBestEffort(Dx);
    VirtIoSndStopHardware(Dx);
    return status;
}

_Use_decl_annotations_
NTSTATUS VirtIoSndHwSendControl(
    PVIRTIOSND_DEVICE_EXTENSION Dx,
    const void *Req,
    ULONG ReqLen,
    void *Resp,
    ULONG RespCap,
    ULONG TimeoutMs,
    ULONG *OutVirtioStatus,
    ULONG *OutRespLen)
{
    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndCtrlSendSync(&Dx->Control, Req, ReqLen, Resp, RespCap, TimeoutMs, OutVirtioStatus, OutRespLen);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndHwSubmitTx(
    PVIRTIOSND_DEVICE_EXTENSION Dx,
    const VOID *Pcm1,
    ULONG Pcm1Bytes,
    const VOID *Pcm2,
    ULONG Pcm2Bytes,
    BOOLEAN AllowSilenceFill)
{
    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    /*
     * TX engine initialization (buffer sizing, pool depth) is stream-specific and
     * currently performed by higher layers (WaveRT stream). Fail clearly if a
     * caller attempts to submit before TxInit has run.
     */
    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0 || Dx->Tx.Queue == NULL || Dx->Tx.Buffers == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndTxSubmitPeriod(&Dx->Tx, Pcm1, Pcm1Bytes, Pcm2, Pcm2Bytes, AllowSilenceFill);
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndHwSubmitTxSg(PVIRTIOSND_DEVICE_EXTENSION Dx, const VIRTIOSND_TX_SEGMENT *Segments, ULONG SegmentCount)
{
    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0 || Dx->Tx.Queue == NULL || Dx->Tx.Buffers == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioSndTxSubmitSg(&Dx->Tx, Segments, SegmentCount);
}

_Use_decl_annotations_
ULONG
VirtIoSndHwDrainTxCompletions(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    if (Dx == NULL) {
        return 0;
    }

    if (Dx->Removed) {
        return 0;
    }

    if (!Dx->Started) {
        return 0;
    }

    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0 || Dx->Tx.Queue == NULL || Dx->Tx.Buffers == NULL) {
        return 0;
    }

    return VirtioSndTxDrainCompletions(&Dx->Tx);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndInitTxEngine(PVIRTIOSND_DEVICE_EXTENSION Dx, ULONG MaxPeriodBytes, ULONG BufferCount, BOOLEAN SuppressInterrupts)
{
    NTSTATUS status;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) != 0) {
#ifdef STATUS_ALREADY_INITIALIZED
        return STATUS_ALREADY_INITIALIZED;
#else
        return STATUS_INVALID_DEVICE_STATE;
#endif
    }

    status = VirtioSndTxInit(&Dx->Tx, &Dx->DmaCtx, &Dx->Queues[VIRTIOSND_QUEUE_TX], MaxPeriodBytes, BufferCount, SuppressInterrupts);
    if (NT_SUCCESS(status)) {
        InterlockedExchange(&Dx->TxEngineInitialized, 1);
    } else {
        RtlZeroMemory(&Dx->Tx, sizeof(Dx->Tx));
        InterlockedExchange(&Dx->TxEngineInitialized, 0);
    }

    return status;
}

_Use_decl_annotations_
VOID VirtIoSndUninitTxEngine(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    LARGE_INTEGER delay;
    ULONG attempts;
    ULONG drained;
    ULONG inflight;
    KIRQL oldIrql;

    if (Dx == NULL) {
        return;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return;
    }
    if (InterlockedCompareExchange(&Dx->TxEngineInitialized, 0, 0) == 0 && Dx->Tx.Queue == NULL && Dx->Tx.Buffers == NULL) {
        return;
    }

    InterlockedExchange(&Dx->TxEngineInitialized, 0);

    delay.QuadPart = -10 * 1000; /* 1ms */
    while (InterlockedCompareExchange(&Dx->Intx.DpcInFlight, 0, 0) > 0) {
        KeDelayExecutionThread(KernelMode, FALSE, &delay);
    }

    /*
     * Drain completions before freeing the TX buffer pool so the txq does not
     * retain cookies that point to freed allocations. txq interrupts are
     * suppressed for Aero (to avoid immediate-completion interrupt storms), so
     * we must poll for completion during teardown.
     */
    inflight = 0;
    for (attempts = 0; attempts < 200; ++attempts) {
        drained = VirtioSndTxDrainCompletions(&Dx->Tx);

        KeAcquireSpinLock(&Dx->Tx.Lock, &oldIrql);
        inflight = Dx->Tx.InflightCount;
        KeReleaseSpinLock(&Dx->Tx.Lock, oldIrql);

        if (inflight == 0) {
            break;
        }

        /* If no progress was made, back off briefly to avoid busy-waiting. */
        if (drained == 0) {
            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    }

    if (inflight != 0) {
        VIRTIOSND_TRACE_ERROR("tx engine teardown: %lu buffer(s) still inflight\n", inflight);
    }

    VirtioSndTxUninit(&Dx->Tx);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndInitRxEngine(PVIRTIOSND_DEVICE_EXTENSION Dx, ULONG RequestCount)
{
    NTSTATUS status;

    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }
    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (InterlockedCompareExchange(&Dx->RxEngineInitialized, 0, 0) != 0) {
#ifdef STATUS_ALREADY_INITIALIZED
        return STATUS_ALREADY_INITIALIZED;
#else
        return STATUS_INVALID_DEVICE_STATE;
#endif
    }

    status = VirtIoSndRxInit(&Dx->Rx, &Dx->DmaCtx, &Dx->Queues[VIRTIOSND_QUEUE_RX], RequestCount);
    if (NT_SUCCESS(status)) {
        InterlockedExchange(&Dx->RxEngineInitialized, 1);
    } else {
        RtlZeroMemory(&Dx->Rx, sizeof(Dx->Rx));
        InterlockedExchange(&Dx->RxEngineInitialized, 0);
    }

    return status;
}

_Use_decl_annotations_
VOID VirtIoSndUninitRxEngine(PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    LARGE_INTEGER delay;
    ULONG attempts;
    ULONG drained;
    ULONG inflight;
    KIRQL oldIrql;

    if (Dx == NULL) {
        return;
    }
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return;
    }
    if (InterlockedCompareExchange(&Dx->RxEngineInitialized, 0, 0) == 0 && Dx->Rx.Queue == NULL && Dx->Rx.Requests == NULL) {
        return;
    }

    InterlockedExchange(&Dx->RxEngineInitialized, 0);

    delay.QuadPart = -10 * 1000; /* 1ms */
    while (InterlockedCompareExchange(&Dx->Intx.DpcInFlight, 0, 0) > 0) {
        KeDelayExecutionThread(KernelMode, FALSE, &delay);
    }

    /*
     * Drain completions before freeing RX request contexts so the rxq does not
     * retain cookies that point to freed allocations.
     *
     * We poll here because capture completions may have interrupts suppressed in
     * the WaveRT capture path.
     */
    inflight = 0;
    for (attempts = 0; attempts < 200; ++attempts) {
        drained = VirtIoSndRxDrainCompletions(&Dx->Rx, NULL, NULL);

        KeAcquireSpinLock(&Dx->Rx.Lock, &oldIrql);
        inflight = Dx->Rx.InflightCount;
        KeReleaseSpinLock(&Dx->Rx.Lock, oldIrql);

        if (inflight == 0) {
            break;
        }

        if (drained == 0) {
            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
    }

    if (inflight != 0) {
        VIRTIOSND_TRACE_ERROR("rx engine teardown: %lu request(s) still inflight\n", inflight);
    }

    VirtIoSndRxUninit(&Dx->Rx);
}

_Use_decl_annotations_
VOID VirtIoSndHwSetRxCompletionCallback(PVIRTIOSND_DEVICE_EXTENSION Dx, EVT_VIRTIOSND_RX_COMPLETION* Callback, void* Context)
{
    if (Dx == NULL) {
        return;
    }
    if (Dx->Removed || !Dx->Started) {
        return;
    }
    if (InterlockedCompareExchange(&Dx->RxEngineInitialized, 0, 0) == 0) {
        return;
    }

    VirtIoSndRxSetCompletionCallback(&Dx->Rx, Callback, Context);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndHwSubmitRxSg(PVIRTIOSND_DEVICE_EXTENSION Dx, const VIRTIOSND_RX_SEGMENT* Segments, USHORT SegmentCount, void* Cookie)
{
    if (Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->Removed) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Dx->Started) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (InterlockedCompareExchange(&Dx->RxEngineInitialized, 0, 0) == 0 || Dx->Rx.Queue == NULL || Dx->Rx.Requests == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtIoSndRxSubmitSg(&Dx->Rx, Segments, SegmentCount, Cookie);
}

_Use_decl_annotations_
ULONG VirtIoSndHwDrainRxCompletions(PVIRTIOSND_DEVICE_EXTENSION Dx, EVT_VIRTIOSND_RX_COMPLETION* Callback, void* Context)
{
    if (Dx == NULL) {
        return 0;
    }

    if (Dx->Removed) {
        return 0;
    }

    if (!Dx->Started) {
        return 0;
    }

    if (InterlockedCompareExchange(&Dx->RxEngineInitialized, 0, 0) == 0 || Dx->Rx.Queue == NULL || Dx->Rx.Requests == NULL) {
        return 0;
    }

    return VirtIoSndRxDrainCompletions(&Dx->Rx, Callback, Context);
}
