#include "../include/aerovblk.h"

#ifndef PCI_TYPE0_ADDRESSES
#define PCI_TYPE0_ADDRESSES 6
#endif

static VOID AerovblkCompleteSrb(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ UCHAR srbStatus);

static VOID
AerovblkSetSense(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt,
                 _Inout_ PSCSI_REQUEST_BLOCK srb,
                 _In_ UCHAR senseKey,
                 _In_ UCHAR asc,
                 _In_ UCHAR ascq)
{
    SENSE_DATA sense;
    ULONG copyLen;

    RtlZeroMemory(&sense, sizeof(sense));
    sense.ErrorCode = 0x70;
    sense.SenseKey = senseKey;
    sense.AdditionalSenseCode = asc;
    sense.AdditionalSenseCodeQualifier = ascq;
    sense.AdditionalSenseLength = 0x0A;

    devExt->LastSense = sense;

    if (srb->SenseInfoBuffer != NULL && srb->SenseInfoBufferLength != 0) {
        copyLen = (srb->SenseInfoBufferLength < sizeof(sense)) ? srb->SenseInfoBufferLength : sizeof(sense);
        RtlCopyMemory(srb->SenseInfoBuffer, &sense, copyLen);
    }

    srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
}

static VOID AerovblkCompleteSrb(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ UCHAR srbStatus)
{
    srb->SrbStatus = srbStatus;
    if ((srbStatus & SRB_STATUS_STATUS_MASK) == SRB_STATUS_SUCCESS) {
        srb->ScsiStatus = SCSISTAT_GOOD;
    }

    StorPortNotification(RequestComplete, deviceExtension, srb);
}

/* -------------------------------------------------------------------------- */
/* SCSI / big-endian helpers                                                   */
/* -------------------------------------------------------------------------- */

static __forceinline ULONGLONG AerovblkBe64ToCpu(_In_reads_bytes_(8) const UCHAR *p)
{
    return ((ULONGLONG)p[0] << 56) | ((ULONGLONG)p[1] << 48) | ((ULONGLONG)p[2] << 40) | ((ULONGLONG)p[3] << 32) |
           ((ULONGLONG)p[4] << 24) | ((ULONGLONG)p[5] << 16) | ((ULONGLONG)p[6] << 8) | ((ULONGLONG)p[7]);
}

static __forceinline ULONG AerovblkBe32ToCpu(_In_reads_bytes_(4) const UCHAR *p)
{
    return ((ULONG)p[0] << 24) | ((ULONG)p[1] << 16) | ((ULONG)p[2] << 8) | ((ULONG)p[3]);
}

static __forceinline USHORT AerovblkBe16ToCpu(_In_reads_bytes_(2) const UCHAR *p)
{
    return (USHORT)(((USHORT)p[0] << 8) | (USHORT)p[1]);
}

static VOID AerovblkWriteBe32(_Out_writes_bytes_(4) UCHAR *p, _In_ ULONG v)
{
    p[0] = (UCHAR)(v >> 24);
    p[1] = (UCHAR)(v >> 16);
    p[2] = (UCHAR)(v >> 8);
    p[3] = (UCHAR)v;
}

static VOID AerovblkWriteBe64(_Out_writes_bytes_(8) UCHAR *p, _In_ ULONGLONG v)
{
    p[0] = (UCHAR)(v >> 56);
    p[1] = (UCHAR)(v >> 48);
    p[2] = (UCHAR)(v >> 40);
    p[3] = (UCHAR)(v >> 32);
    p[4] = (UCHAR)(v >> 24);
    p[5] = (UCHAR)(v >> 16);
    p[6] = (UCHAR)(v >> 8);
    p[7] = (UCHAR)v;
}

static __forceinline ULONG AerovblkSectorsPerLogicalBlock(_In_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    if (devExt->LogicalSectorSize < AEROVBLK_LOGICAL_SECTOR_SIZE) {
        return 1;
    }
    if ((devExt->LogicalSectorSize % AEROVBLK_LOGICAL_SECTOR_SIZE) != 0) {
        return 1;
    }
    return devExt->LogicalSectorSize / AEROVBLK_LOGICAL_SECTOR_SIZE;
}

static __forceinline ULONGLONG AerovblkTotalLogicalBlocks(_In_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    ULONGLONG capBytes;

    if (devExt->LogicalSectorSize == 0) {
        return 0;
    }

    capBytes = devExt->CapacitySectors * (ULONGLONG)AEROVBLK_LOGICAL_SECTOR_SIZE;
    return capBytes / (ULONGLONG)devExt->LogicalSectorSize;
}

/* -------------------------------------------------------------------------- */
/* Modern virtio-pci (MMIO) helpers                                            */
/* -------------------------------------------------------------------------- */

#define AEROVBLK_PCI_RESET_TIMEOUT_US 1000000u
#define AEROVBLK_PCI_RESET_POLL_DELAY_US 1000u
#define AEROVBLK_PCI_CONFIG_MAX_READ_RETRIES 10u

static __forceinline VOID AerovblkCommonCfgLock(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Out_ PKIRQL oldIrql)
{
    KeAcquireSpinLock(&devExt->CommonCfgLock, oldIrql);
}

static __forceinline VOID AerovblkCommonCfgUnlock(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ KIRQL oldIrql)
{
    KeReleaseSpinLock(&devExt->CommonCfgLock, oldIrql);
}

static __forceinline UCHAR AerovblkReadDeviceStatus(_In_ const PAEROVBLK_DEVICE_EXTENSION devExt)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)&devExt->CommonCfg->device_status);
}

static __forceinline VOID AerovblkWriteDeviceStatus(_In_ const PAEROVBLK_DEVICE_EXTENSION devExt, _In_ UCHAR status)
{
    WRITE_REGISTER_UCHAR((volatile UCHAR *)&devExt->CommonCfg->device_status, status);
}

static VOID AerovblkResetDevice(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    ULONG waitedUs;

    if (devExt->CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    AerovblkWriteDeviceStatus(devExt, 0);
    KeMemoryBarrier();

    for (waitedUs = 0; waitedUs < AEROVBLK_PCI_RESET_TIMEOUT_US; waitedUs += AEROVBLK_PCI_RESET_POLL_DELAY_US) {
        if (AerovblkReadDeviceStatus(devExt) == 0) {
            KeMemoryBarrier();
            return;
        }

        KeStallExecutionProcessor(AEROVBLK_PCI_RESET_POLL_DELAY_US);
    }
}

static VOID AerovblkAddStatus(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ UCHAR bits)
{
    UCHAR status;

    if (devExt->CommonCfg == NULL) {
        return;
    }

    KeMemoryBarrier();
    status = AerovblkReadDeviceStatus(devExt);
    status |= bits;
    AerovblkWriteDeviceStatus(devExt, status);
    KeMemoryBarrier();
}

static UCHAR AerovblkGetStatus(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    if (devExt->CommonCfg == NULL) {
        return 0;
    }

    KeMemoryBarrier();
    return AerovblkReadDeviceStatus(devExt);
}

static VOID AerovblkFailDevice(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    AerovblkAddStatus(devExt, VIRTIO_STATUS_FAILED);
}

static UINT64 AerovblkReadDeviceFeaturesLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    ULONG lo;
    ULONG hi;

    NT_ASSERT(devExt->CommonCfg != NULL);

    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->device_feature_select, 0);
    KeMemoryBarrier();
    lo = READ_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->device_feature);

    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->device_feature_select, 1);
    KeMemoryBarrier();
    hi = READ_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->device_feature);

    return ((UINT64)hi << 32) | (UINT64)lo;
}

static VOID AerovblkWriteDriverFeaturesLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ UINT64 features)
{
    ULONG lo = (ULONG)(features & 0xFFFFFFFFui64);
    ULONG hi = (ULONG)(features >> 32);

    NT_ASSERT(devExt->CommonCfg != NULL);

    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->driver_feature_select, 0);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->driver_feature, lo);

    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->driver_feature_select, 1);
    KeMemoryBarrier();
    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->driver_feature, hi);

    KeMemoryBarrier();
}

static UINT64 AerovblkReadDeviceFeatures(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    KIRQL oldIrql;
    UINT64 features;

    AerovblkCommonCfgLock(devExt, &oldIrql);
    features = AerovblkReadDeviceFeaturesLocked(devExt);
    AerovblkCommonCfgUnlock(devExt, oldIrql);

    return features;
}

static USHORT AerovblkReadQueueNotifyOffLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ USHORT queueIndex)
{
    NT_ASSERT(devExt->CommonCfg != NULL);
    WRITE_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_select, queueIndex);
    KeMemoryBarrier();
    return READ_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_notify_off);
}

static USHORT AerovblkReadQueueSizeLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ USHORT queueIndex)
{
    NT_ASSERT(devExt->CommonCfg != NULL);
    WRITE_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_select, queueIndex);
    KeMemoryBarrier();
    return READ_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_size);
}

static VOID AerovblkNotifyQueue0(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    if (devExt->QueueNotify == NULL) {
        return;
    }

    WRITE_REGISTER_USHORT((volatile USHORT *)devExt->QueueNotify, (USHORT)AEROVBLK_QUEUE_INDEX);
    KeMemoryBarrier();
}

static UCHAR AerovblkReadCfg8(_In_ volatile const VOID *base, _In_ ULONG offset)
{
    return READ_REGISTER_UCHAR((volatile UCHAR *)((ULONG_PTR)base + offset));
}

static USHORT AerovblkReadCfg16(_In_ volatile const VOID *base, _In_ ULONG offset)
{
    return READ_REGISTER_USHORT((volatile USHORT *)((ULONG_PTR)base + offset));
}

static ULONG AerovblkReadCfg32(_In_ volatile const VOID *base, _In_ ULONG offset)
{
    return READ_REGISTER_ULONG((volatile ULONG *)((ULONG_PTR)base + offset));
}

static VOID AerovblkCopyFromDeviceCfg(_In_ volatile const UCHAR *base,
                                     _In_ ULONG offset,
                                     _Out_writes_bytes_(length) UCHAR *outBytes,
                                     _In_ ULONG length)
{
    ULONG i = 0;

    while (i < length && ((offset + i) & 3u) != 0) {
        outBytes[i] = AerovblkReadCfg8(base, offset + i);
        i++;
    }

    while (length - i >= sizeof(ULONG)) {
        ULONG v32 = AerovblkReadCfg32(base, offset + i);
        RtlCopyMemory(outBytes + i, &v32, sizeof(v32));
        i += sizeof(ULONG);
    }

    while (length - i >= sizeof(USHORT)) {
        USHORT v16 = AerovblkReadCfg16(base, offset + i);
        RtlCopyMemory(outBytes + i, &v16, sizeof(v16));
        i += sizeof(USHORT);
    }

    while (i < length) {
        outBytes[i] = AerovblkReadCfg8(base, offset + i);
        i++;
    }
}

static NTSTATUS AerovblkReadDeviceConfig(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt,
                                        _In_ ULONG offset,
                                        _Out_writes_bytes_(length) PVOID buffer,
                                        _In_ ULONG length)
{
    ULONG attempt;
    UCHAR gen0;
    UCHAR gen1;
    ULONGLONG end;

    if (length == 0) {
        return STATUS_SUCCESS;
    }

    if (devExt->CommonCfg == NULL || devExt->DeviceCfg == NULL || buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    end = (ULONGLONG)offset + (ULONGLONG)length;
    if (end < offset) {
        return STATUS_INVALID_PARAMETER;
    }

    if (devExt->DeviceCfgLength != 0 && end > devExt->DeviceCfgLength) {
        return STATUS_INVALID_PARAMETER;
    }

    for (attempt = 0; attempt < AEROVBLK_PCI_CONFIG_MAX_READ_RETRIES; attempt++) {
        gen0 = READ_REGISTER_UCHAR((volatile UCHAR *)&devExt->CommonCfg->config_generation);
        KeMemoryBarrier();

        AerovblkCopyFromDeviceCfg(devExt->DeviceCfg, offset, (PUCHAR)buffer, length);

        KeMemoryBarrier();
        gen1 = READ_REGISTER_UCHAR((volatile UCHAR *)&devExt->CommonCfg->config_generation);
        KeMemoryBarrier();

        if (gen0 == gen1) {
            return STATUS_SUCCESS;
        }
    }

    return STATUS_IO_TIMEOUT;
}

static BOOLEAN AerovblkComputeQueueNotifyAddr(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ USHORT notifyOff)
{
    ULONGLONG byteOffset;

    if (devExt->NotifyBase == NULL || devExt->NotifyOffMultiplier == 0 || devExt->NotifyLength < sizeof(UINT16)) {
        return FALSE;
    }

    byteOffset = (ULONGLONG)notifyOff * (ULONGLONG)devExt->NotifyOffMultiplier;
    if (byteOffset + sizeof(UINT16) > (ULONGLONG)devExt->NotifyLength) {
        return FALSE;
    }

    devExt->QueueNotify = (volatile UINT16 *)((volatile UCHAR *)devExt->NotifyBase + byteOffset);
    return TRUE;
}

/* -------------------------------------------------------------------------- */
/* Request context management                                                  */
/* -------------------------------------------------------------------------- */

static VOID AerovblkResetRequestContextsLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    ULONG i;
    PAEROVBLK_REQUEST_CONTEXT ctx;

    InitializeListHead(&devExt->FreeRequestList);
    devExt->FreeRequestCount = 0;

    if (devExt->RequestContexts == NULL) {
        return;
    }

    for (i = 0; i < devExt->RequestContextCount; ++i) {
        ctx = &devExt->RequestContexts[i];
        ctx->Srb = NULL;
        ctx->IsWrite = FALSE;
        InsertTailList(&devExt->FreeRequestList, &ctx->Link);
        devExt->FreeRequestCount++;
    }
}

static VOID AerovblkAbortOutstandingRequestsLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    ULONG i;
    PAEROVBLK_REQUEST_CONTEXT ctx;
    PSCSI_REQUEST_BLOCK srb;

    if (devExt->RequestContexts == NULL) {
        return;
    }

    for (i = 0; i < devExt->RequestContextCount; ++i) {
        ctx = &devExt->RequestContexts[i];
        srb = ctx->Srb;
        if (srb == NULL) {
            continue;
        }

        ctx->Srb = NULL;
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ABORTED_COMMAND, 0x00, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    }

    AerovblkResetRequestContextsLocked(devExt);
}

static BOOLEAN AerovblkAllocateRequestContexts(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    ULONG i;
    ULONG ctxCount;
    PHYSICAL_ADDRESS low;
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS boundary;
    PVOID pageVa;
    ULONG pageLen;
    STOR_PHYSICAL_ADDRESS pagePa;
    PAEROVBLK_REQUEST_CONTEXT ctx;

    if (devExt->RequestContexts != NULL) {
        return TRUE;
    }

    ctxCount = (ULONG)devExt->QueueSize;
    devExt->RequestContextCount = ctxCount;

    devExt->RequestContexts =
        (PAEROVBLK_REQUEST_CONTEXT)StorPortAllocatePool(devExt, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount, 'bVrA');
    if (devExt->RequestContexts == NULL) {
        return FALSE;
    }

    RtlZeroMemory(devExt->RequestContexts, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount);

    InitializeListHead(&devExt->FreeRequestList);
    devExt->FreeRequestCount = 0;

    low.QuadPart = 0;
    high.QuadPart = -1;
    boundary.QuadPart = 0;

    for (i = 0; i < ctxCount; ++i) {
        pageVa = StorPortAllocateContiguousMemorySpecifyCache(devExt, PAGE_SIZE, low, high, boundary, MmNonCached);
        if (pageVa == NULL) {
            return FALSE;
        }

        pageLen = PAGE_SIZE;
        pagePa = StorPortGetPhysicalAddress(devExt, NULL, pageVa, &pageLen);
        if (pageLen < PAGE_SIZE) {
            return FALSE;
        }

        RtlZeroMemory(pageVa, PAGE_SIZE);

        ctx = &devExt->RequestContexts[i];
        InitializeListHead(&ctx->Link);

        ctx->SharedPageVa = pageVa;
        ctx->SharedPagePa = (UINT64)pagePa.QuadPart;

        ctx->ReqHdr = (volatile VIRTIO_BLK_REQ_HDR *)((PUCHAR)pageVa + AEROVBLK_CTX_HDR_OFFSET);
        ctx->ReqHdrPa = ctx->SharedPagePa + AEROVBLK_CTX_HDR_OFFSET;

        ctx->StatusByte = (volatile UCHAR *)((PUCHAR)pageVa + AEROVBLK_CTX_STATUS_OFFSET);
        ctx->StatusPa = ctx->SharedPagePa + AEROVBLK_CTX_STATUS_OFFSET;

        ctx->Srb = NULL;
        ctx->IsWrite = FALSE;

        InsertTailList(&devExt->FreeRequestList, &ctx->Link);
        devExt->FreeRequestCount++;
    }

    return TRUE;
}

static BOOLEAN AerovblkAllocateVirtqueue(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    size_t vqBytes;
    size_t ringBytes;
    ULONG ringAlloc;
    size_t indirectStride;
    size_t indirectBytes;
    ULONG indirectAlloc;
    NTSTATUS st;
    PHYSICAL_ADDRESS low;
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS boundary;
    ULONG paLen;
    STOR_PHYSICAL_ADDRESS pa;

    if (devExt->Vq != NULL) {
        return TRUE;
    }

    if (devExt->QueueSize == 0) {
        return FALSE;
    }

    vqBytes = VirtqSplitStateSize(devExt->QueueSize);
    devExt->Vq = (VIRTQ_SPLIT *)StorPortAllocatePool(devExt, (ULONG)vqBytes, 'qVrA');
    if (devExt->Vq == NULL) {
        return FALSE;
    }

    ringBytes = VirtqSplitRingMemSize(devExt->QueueSize, PAGE_SIZE, FALSE);
    if (ringBytes == 0) {
        return FALSE;
    }
    ringAlloc = (ULONG)ROUND_TO_PAGES(ringBytes);

    low.QuadPart = 0;
    high.QuadPart = -1;
    boundary.QuadPart = 0;

    devExt->RingVa = StorPortAllocateContiguousMemorySpecifyCache(devExt, ringAlloc, low, high, boundary, MmNonCached);
    if (devExt->RingVa == NULL) {
        return FALSE;
    }
    devExt->RingSize = ringAlloc;

    paLen = ringAlloc;
    pa = StorPortGetPhysicalAddress(devExt, NULL, devExt->RingVa, &paLen);
    if (paLen < ringAlloc) {
        return FALSE;
    }
    devExt->RingPa = (UINT64)pa.QuadPart;

    devExt->IndirectTableCount = devExt->QueueSize;
    devExt->IndirectMaxDesc = (USHORT)(AEROVBLK_MAX_DATA_SG + 2u);

    indirectStride = sizeof(VIRTQ_DESC) * (size_t)devExt->IndirectMaxDesc;
    indirectBytes = indirectStride * (size_t)devExt->IndirectTableCount;
    indirectAlloc = (ULONG)ROUND_TO_PAGES(indirectBytes);

    devExt->IndirectPoolVa =
        StorPortAllocateContiguousMemorySpecifyCache(devExt, indirectAlloc, low, high, boundary, MmNonCached);
    if (devExt->IndirectPoolVa == NULL) {
        return FALSE;
    }
    devExt->IndirectPoolSize = indirectAlloc;

    paLen = indirectAlloc;
    pa = StorPortGetPhysicalAddress(devExt, NULL, devExt->IndirectPoolVa, &paLen);
    if (paLen < indirectAlloc) {
        return FALSE;
    }
    devExt->IndirectPoolPa = (UINT64)pa.QuadPart;

    st = VirtqSplitInit(devExt->Vq,
                        devExt->QueueSize,
                        FALSE,
                        TRUE,
                        devExt->RingVa,
                        devExt->RingPa,
                        PAGE_SIZE,
                        devExt->IndirectPoolVa,
                        devExt->IndirectPoolPa,
                        devExt->IndirectTableCount,
                        devExt->IndirectMaxDesc);
    if (!NT_SUCCESS(st)) {
        AEROVBLK_LOG("VirtqSplitInit failed: 0x%08lx", st);
        return FALSE;
    }

    /*
     * Contract v1 requires indirect descriptors; prefer indirect for all I/O to
     * keep the ring descriptor table maximally available.
     */
    devExt->Vq->indirect_threshold = 0;

    return TRUE;
}

static NTSTATUS AerovblkSetupQueue0(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    KIRQL oldIrql;
    USHORT size;
    USHORT notifyOff;
    USHORT enabled;

    if (devExt->CommonCfg == NULL || devExt->Vq == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    AerovblkCommonCfgLock(devExt, &oldIrql);

    /* Ensure MSI-X is disabled so INTx+ISR works even if MSI-X is present. */
    WRITE_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->msix_config, (USHORT)0xFFFF);

    WRITE_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_select, (USHORT)AEROVBLK_QUEUE_INDEX);
    KeMemoryBarrier();

    size = READ_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_size);
    notifyOff = READ_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_notify_off);

    /* Disable per-queue MSI-X vector as well (optional in contract v1). */
    WRITE_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_msix_vector, (USHORT)0xFFFF);

    if (size != devExt->QueueSize) {
        AerovblkCommonCfgUnlock(devExt, oldIrql);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (notifyOff != 0) {
        AerovblkCommonCfgUnlock(devExt, oldIrql);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->queue_desc_lo, (ULONG)(devExt->Vq->desc_pa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->queue_desc_hi, (ULONG)(devExt->Vq->desc_pa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->queue_avail_lo, (ULONG)(devExt->Vq->avail_pa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->queue_avail_hi, (ULONG)(devExt->Vq->avail_pa >> 32));

    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->queue_used_lo, (ULONG)(devExt->Vq->used_pa & 0xFFFFFFFFui64));
    WRITE_REGISTER_ULONG((volatile ULONG *)&devExt->CommonCfg->queue_used_hi, (ULONG)(devExt->Vq->used_pa >> 32));

    KeMemoryBarrier();

    WRITE_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_enable, 1);
    enabled = READ_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->queue_enable);

    AerovblkCommonCfgUnlock(devExt, oldIrql);

    if (enabled != 1) {
        return STATUS_IO_DEVICE_ERROR;
    }

    if (!AerovblkComputeQueueNotifyAddr(devExt, notifyOff)) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    return STATUS_SUCCESS;
}

static BOOLEAN AerovblkDeviceBringUp(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ BOOLEAN allocateResources)
{
    UINT64 requiredFeatures;
    UINT64 deviceFeatures;
    UINT64 negotiated;
    UCHAR status;
    VIRTIO_BLK_CONFIG cfg;
    NTSTATUS st;
    STOR_LOCK_HANDLE lock;

    /*
     * Reset the device into a known state. This also disables all queues and
     * clears pending interrupts per the contract.
     */
    AerovblkResetDevice(devExt);

    if (!allocateResources) {
        StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
        AerovblkAbortOutstandingRequestsLocked(devExt);
        if (devExt->Vq != NULL) {
            VirtqSplitReset(devExt->Vq);
        }
        StorPortReleaseSpinLock(devExt, &lock);
    }

    requiredFeatures = VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_BLK_F_SEG_MAX | VIRTIO_BLK_F_BLK_SIZE |
                       VIRTIO_BLK_F_FLUSH;

    AerovblkAddStatus(devExt, VIRTIO_STATUS_ACKNOWLEDGE);
    AerovblkAddStatus(devExt, VIRTIO_STATUS_DRIVER);

    deviceFeatures = AerovblkReadDeviceFeatures(devExt);
    if ((deviceFeatures & requiredFeatures) != requiredFeatures) {
        AEROVBLK_LOG("missing required features (device=0x%I64x required=0x%I64x)", deviceFeatures, requiredFeatures);
        AerovblkFailDevice(devExt);
        return FALSE;
    }
    if ((deviceFeatures & VIRTIO_F_RING_EVENT_IDX) != 0) {
        AEROVBLK_LOG("device offers EVENT_IDX (0x%I64x), not supported by contract v1", deviceFeatures);
        AerovblkFailDevice(devExt);
        return FALSE;
    }

    negotiated = requiredFeatures;
    devExt->NegotiatedFeatures = negotiated;
    devExt->SupportsFlush = TRUE;

    {
        KIRQL oldIrql;
        AerovblkCommonCfgLock(devExt, &oldIrql);
        AerovblkWriteDriverFeaturesLocked(devExt, negotiated);
        AerovblkCommonCfgUnlock(devExt, oldIrql);
    }

    AerovblkAddStatus(devExt, VIRTIO_STATUS_FEATURES_OK);
    status = AerovblkGetStatus(devExt);
    if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
        AEROVBLK_LOG("device rejected FEATURES_OK (status=0x%02x)", status);
        AerovblkFailDevice(devExt);
        return FALSE;
    }

    if (allocateResources) {
        if (!AerovblkAllocateVirtqueue(devExt)) {
            AEROVBLK_LOG("%s", "failed to allocate virtqueue resources");
            AerovblkFailDevice(devExt);
            return FALSE;
        }

        if (!AerovblkAllocateRequestContexts(devExt)) {
            AEROVBLK_LOG("%s", "failed to allocate request contexts");
            AerovblkFailDevice(devExt);
            return FALSE;
        }
    }

    /*
     * Program queue0 and enable it. Queue addresses must be written after
     * FEATURES_OK and before DRIVER_OK.
     */
    st = AerovblkSetupQueue0(devExt);
    if (!NT_SUCCESS(st)) {
        AEROVBLK_LOG("AerovblkSetupQueue0 failed: 0x%08lx", st);
        AerovblkFailDevice(devExt);
        return FALSE;
    }

    RtlZeroMemory(&cfg, sizeof(cfg));
    st = AerovblkReadDeviceConfig(devExt, 0, &cfg, sizeof(cfg));
    if (!NT_SUCCESS(st)) {
        AEROVBLK_LOG("AerovblkReadDeviceConfig failed: 0x%08lx", st);
        AerovblkFailDevice(devExt);
        return FALSE;
    }

    /* Contract v1: size_max is not used and must be 0. */
    if (cfg.SizeMax != 0) {
        AEROVBLK_LOG("contract violation: size_max=%lu (expected 0)", cfg.SizeMax);
        AerovblkFailDevice(devExt);
        return FALSE;
    }

    devExt->CapacitySectors = cfg.Capacity;

    devExt->LogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
    if (cfg.BlkSize >= AEROVBLK_LOGICAL_SECTOR_SIZE && (cfg.BlkSize % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0 &&
        ((cfg.BlkSize & (cfg.BlkSize - 1)) == 0)) {
        devExt->LogicalSectorSize = cfg.BlkSize;
    }

    devExt->SegMax = cfg.SegMax;
    if (devExt->SegMax == 0) {
        AEROVBLK_LOG("%s", "contract violation: seg_max=0");
        AerovblkFailDevice(devExt);
        return FALSE;
    }
    if (devExt->SegMax > AEROVBLK_MAX_DATA_SG) {
        devExt->SegMax = AEROVBLK_MAX_DATA_SG;
    }

    devExt->SizeMax = cfg.SizeMax;

    AEROVBLK_LOG("capacity_sectors=%I64u blk_size=%lu seg_max=%lu", devExt->CapacitySectors, devExt->LogicalSectorSize, devExt->SegMax);

    AerovblkAddStatus(devExt, VIRTIO_STATUS_DRIVER_OK);
    StorPortNotification(NextRequest, devExt, NULL);
    return TRUE;
}

/* -------------------------------------------------------------------------- */
/* Virtio request submission                                                   */
/* -------------------------------------------------------------------------- */

static BOOLEAN AerovblkQueueRequest(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt,
                                   _Inout_ PSCSI_REQUEST_BLOCK srb,
                                   _In_ ULONG reqType,
                                   _In_ ULONGLONG startSector,
                                   _In_opt_ PSTOR_SCATTER_GATHER_LIST sg,
                                   _In_ BOOLEAN isWrite)
{
    STOR_LOCK_HANDLE lock;
    LIST_ENTRY *entry;
    PAEROVBLK_REQUEST_CONTEXT ctx;
    ULONG sgCount;
    USHORT totalDesc;
    VIRTQ_SG sgList[AEROVBLK_MAX_DATA_SG + 2u];
    USHORT head;
    NTSTATUS st;
    BOOLEAN shouldKick;

    if (devExt->Vq == NULL) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
        return TRUE;
    }

    sgCount = (sg == NULL) ? 0 : sg->NumberOfElements;
    if (sgCount > AEROVBLK_MAX_DATA_SG) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return TRUE;
    }
    if (devExt->SegMax != 0 && sgCount > devExt->SegMax) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return TRUE;
    }

    totalDesc = (USHORT)(sgCount + 2u);
    if (totalDesc > devExt->IndirectMaxDesc) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return TRUE;
    }

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

    if (devExt->Removed) {
        StorPortReleaseSpinLock(devExt, &lock);
        AerovblkSetSense(devExt, srb, SCSI_SENSE_NOT_READY, 0x04, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
        return TRUE;
    }

    if (devExt->FreeRequestCount == 0 || IsListEmpty(&devExt->FreeRequestList)) {
        StorPortReleaseSpinLock(devExt, &lock);
        return FALSE;
    }

    entry = RemoveHeadList(&devExt->FreeRequestList);
    devExt->FreeRequestCount--;
    ctx = CONTAINING_RECORD(entry, AEROVBLK_REQUEST_CONTEXT, Link);

    ctx->Srb = srb;
    ctx->IsWrite = isWrite;

    ctx->ReqHdr->Type = reqType;
    ctx->ReqHdr->Reserved = 0;
    ctx->ReqHdr->Sector = startSector;
    *ctx->StatusByte = 0xFF;

    sgList[0].addr = ctx->ReqHdrPa;
    sgList[0].len = sizeof(VIRTIO_BLK_REQ_HDR);
    sgList[0].write = FALSE;

    if (sg != NULL && sgCount != 0) {
        ULONG i;
        for (i = 0; i < sgCount; i++) {
            sgList[1 + i].addr = (UINT64)sg->Elements[i].PhysicalAddress.QuadPart;
            sgList[1 + i].len = sg->Elements[i].Length;
            sgList[1 + i].write = isWrite ? FALSE : TRUE;
        }
    }

    sgList[1 + sgCount].addr = ctx->StatusPa;
    sgList[1 + sgCount].len = 1;
    sgList[1 + sgCount].write = TRUE;

    head = VIRTQ_SPLIT_NO_DESC;
    st = VirtqSplitAddBuffer(devExt->Vq, sgList, totalDesc, ctx, &head);
    if (!NT_SUCCESS(st)) {
        ctx->Srb = NULL;
        InsertTailList(&devExt->FreeRequestList, &ctx->Link);
        devExt->FreeRequestCount++;
        StorPortReleaseSpinLock(devExt, &lock);

        if (st == STATUS_INSUFFICIENT_RESOURCES) {
            return FALSE;
        }

        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
        return TRUE;
    }

    VirtqSplitPublish(devExt->Vq, head);

    shouldKick = VirtqSplitKickPrepare(devExt->Vq);
    if (shouldKick) {
        AerovblkNotifyQueue0(devExt);
        VirtqSplitKickCommit(devExt->Vq);
    }

    StorPortReleaseSpinLock(devExt, &lock);
    StorPortNotification(NextRequest, devExt, NULL);
    return TRUE;
}

/* -------------------------------------------------------------------------- */
/* SCSI command handling                                                       */
/* -------------------------------------------------------------------------- */

static VOID AerovblkHandleInquiry(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    ULONG allocLen;
    UCHAR evpd;
    UCHAR pageCode;
    PUCHAR out;
    ULONG outLen;
    INQUIRYDATA inq;

    allocLen = srb->Cdb[4];
    evpd = (srb->Cdb[1] & 0x01) ? 1 : 0;
    pageCode = srb->Cdb[2];

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    out = (PUCHAR)srb->DataBuffer;
    outLen = (srb->DataTransferLength < allocLen) ? srb->DataTransferLength : allocLen;
    RtlZeroMemory(out, outLen);

    if (evpd) {
        if (outLen < 4) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return;
        }

        out[0] = DIRECT_ACCESS_DEVICE;
        out[1] = pageCode;
        out[2] = 0;
        out[3] = 0;

        if (pageCode == 0x00) {
            const UCHAR pages[] = {0x00, 0x80, 0x83};
            ULONG copy;
            copy = (outLen - 4 < sizeof(pages)) ? (outLen - 4) : sizeof(pages);
            out[3] = (UCHAR)copy;
            if (copy != 0) {
                RtlCopyMemory(out + 4, pages, copy);
            }
            srb->DataTransferLength = 4 + copy;
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return;
        }

        if (pageCode == 0x80) {
            static const CHAR serial[] = "00000000";
            ULONG serialLen;
            ULONG copy;
            serialLen = sizeof(serial) - 1;
            copy = (outLen - 4 < serialLen) ? (outLen - 4) : serialLen;
            out[3] = (UCHAR)copy;
            if (copy != 0) {
                RtlCopyMemory(out + 4, serial, copy);
            }
            srb->DataTransferLength = 4 + copy;
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return;
        }

        if (pageCode == 0x83) {
            srb->DataTransferLength = 4;
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return;
        }

        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    RtlZeroMemory(&inq, sizeof(inq));
    inq.DeviceType = DIRECT_ACCESS_DEVICE;
    inq.Versions = 5;
    inq.ResponseDataFormat = 2;
    inq.AdditionalLength = sizeof(INQUIRYDATA) - 5;
    RtlCopyMemory(inq.VendorId, "AERO    ", 8);
    RtlCopyMemory(inq.ProductId, "VIRTIO-BLK      ", 16);
    RtlCopyMemory(inq.ProductRevisionLevel, "0001", 4);

    if (outLen > sizeof(inq)) {
        outLen = sizeof(inq);
    }

    RtlCopyMemory(out, &inq, outLen);
    srb->DataTransferLength = outLen;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleReadCapacity10(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    PUCHAR out;
    ULONGLONG totalBlocks;
    ULONGLONG lastLba;
    ULONG lastLba32;

    if (srb->DataBuffer == NULL || srb->DataTransferLength < 8) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    out = (PUCHAR)srb->DataBuffer;
    RtlZeroMemory(out, 8);

    totalBlocks = AerovblkTotalLogicalBlocks(devExt);
    lastLba = (totalBlocks == 0) ? 0 : (totalBlocks - 1);

    lastLba32 = (lastLba > 0xFFFFFFFFull) ? 0xFFFFFFFFu : (ULONG)lastLba;
    AerovblkWriteBe32(out + 0, lastLba32);
    AerovblkWriteBe32(out + 4, devExt->LogicalSectorSize);
    srb->DataTransferLength = 8;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleReadCapacity16(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    ULONG allocLen;
    ULONG outLen;
    PUCHAR out;
    ULONGLONG totalBlocks;
    ULONGLONG lastLba;

    allocLen = AerovblkBe32ToCpu(&srb->Cdb[10]);

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    outLen = (srb->DataTransferLength < allocLen) ? srb->DataTransferLength : allocLen;
    if (outLen < 12) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return;
    }

    out = (PUCHAR)srb->DataBuffer;
    RtlZeroMemory(out, outLen);

    totalBlocks = AerovblkTotalLogicalBlocks(devExt);
    lastLba = (totalBlocks == 0) ? 0 : (totalBlocks - 1);

    AerovblkWriteBe64(out + 0, lastLba);
    AerovblkWriteBe32(out + 8, devExt->LogicalSectorSize);

    srb->DataTransferLength = (outLen < 32) ? outLen : 32;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleModeSense(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ BOOLEAN mode10)
{
    UCHAR pageCode;
    ULONG allocLen;
    PUCHAR out;
    ULONG outLen;
    UCHAR cachePage[20];
    ULONG payloadLen;
    ULONG copy;

    UNREFERENCED_PARAMETER(devExt);

    pageCode = srb->Cdb[2] & 0x3F;
    allocLen = mode10 ? AerovblkBe16ToCpu(&srb->Cdb[7]) : srb->Cdb[4];

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    out = (PUCHAR)srb->DataBuffer;
    outLen = (srb->DataTransferLength < allocLen) ? srb->DataTransferLength : allocLen;
    RtlZeroMemory(out, outLen);

    RtlZeroMemory(cachePage, sizeof(cachePage));
    cachePage[0] = 0x08;
    cachePage[1] = 0x12;
    cachePage[2] = 0x04;

    payloadLen = 0;
    if (pageCode == 0x3F || pageCode == 0x08) {
        payloadLen = sizeof(cachePage);
    }

    if (mode10) {
        USHORT modeDataLen;
        if (outLen < 8) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return;
        }

        modeDataLen = (USHORT)(6 + payloadLen);
        out[0] = (UCHAR)(modeDataLen >> 8);
        out[1] = (UCHAR)modeDataLen;

        copy = payloadLen;
        if (copy > outLen - 8) {
            copy = outLen - 8;
        }

        if (copy != 0) {
            RtlCopyMemory(out + 8, cachePage, copy);
        }

        srb->DataTransferLength = 8 + copy;
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return;
    }

    if (outLen < 4) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return;
    }

    out[0] = (UCHAR)(3 + payloadLen);

    copy = payloadLen;
    if (copy > outLen - 4) {
        copy = outLen - 4;
    }

    if (copy != 0) {
        RtlCopyMemory(out + 4, cachePage, copy);
    }

    srb->DataTransferLength = 4 + copy;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleRequestSense(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    ULONG copyLen;

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return;
    }

    copyLen = (srb->DataTransferLength < sizeof(devExt->LastSense)) ? srb->DataTransferLength : sizeof(devExt->LastSense);
    RtlCopyMemory(srb->DataBuffer, &devExt->LastSense, copyLen);
    srb->DataTransferLength = copyLen;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleIoControl(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    PSRB_IO_CONTROL ctrl;
    PAEROVBLK_QUERY_INFO info;
    STOR_LOCK_HANDLE lock;

    if (srb->DataBuffer == NULL || srb->DataTransferLength < sizeof(SRB_IO_CONTROL)) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
        return;
    }

    ctrl = (PSRB_IO_CONTROL)srb->DataBuffer;
    if (RtlCompareMemory(ctrl->Signature, AEROVBLK_SRBIO_SIG, sizeof(ctrl->Signature)) != sizeof(ctrl->Signature)) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
        return;
    }

    if (ctrl->ControlCode != AEROVBLK_IOCTL_QUERY) {
        ctrl->ReturnCode = (ULONG)STATUS_NOT_SUPPORTED;
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
        return;
    }

    if (ctrl->Length < sizeof(AEROVBLK_QUERY_INFO) || srb->DataTransferLength < sizeof(SRB_IO_CONTROL) + sizeof(AEROVBLK_QUERY_INFO)) {
        ctrl->ReturnCode = (ULONG)STATUS_BUFFER_TOO_SMALL;
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
        return;
    }

    info = (PAEROVBLK_QUERY_INFO)((PUCHAR)srb->DataBuffer + sizeof(SRB_IO_CONTROL));

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

    info->NegotiatedFeatures = devExt->NegotiatedFeatures;
    info->QueueSize = devExt->QueueSize;
    info->NumFree = (devExt->Vq != NULL) ? devExt->Vq->num_free : 0;
    info->AvailIdx = (devExt->Vq != NULL) ? devExt->Vq->avail_idx : 0;
    info->UsedIdx = (devExt->Vq != NULL) ? VirtioReadU16((volatile UINT16 *)&devExt->Vq->used->idx) : 0;
    info->IndirectNumFree = (devExt->Vq != NULL) ? devExt->Vq->indirect_num_free : 0;

    StorPortReleaseSpinLock(devExt, &lock);

    ctrl->ReturnCode = 0;
    ctrl->Length = sizeof(AEROVBLK_QUERY_INFO);
    srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + sizeof(AEROVBLK_QUERY_INFO);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleUnsupported(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x20, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
}

/* -------------------------------------------------------------------------- */
/* Storport entry points                                                       */
/* -------------------------------------------------------------------------- */

ULONG DriverEntry(_In_ PDRIVER_OBJECT driverObject, _In_ PUNICODE_STRING registryPath)
{
    HW_INITIALIZATION_DATA initData;

    RtlZeroMemory(&initData, sizeof(initData));
    initData.HwInitializationDataSize = sizeof(initData);
    initData.AdapterInterfaceType = PCIBus;
    initData.DeviceExtensionSize = sizeof(AEROVBLK_DEVICE_EXTENSION);
    initData.HwFindAdapter = AerovblkHwFindAdapter;
    initData.HwInitialize = AerovblkHwInitialize;
    initData.HwStartIo = AerovblkHwStartIo;
    initData.HwInterrupt = AerovblkHwInterrupt;
    initData.HwResetBus = AerovblkHwResetBus;
    initData.HwAdapterControl = AerovblkHwAdapterControl;
    initData.NumberOfAccessRanges = 1; /* BAR0 MMIO only */
    initData.TaggedQueuing = TRUE;
    initData.MultipleRequestPerLu = TRUE;
    initData.AutoRequestSense = FALSE;
    initData.NeedPhysicalAddresses = TRUE;
    initData.MapBuffers = TRUE;

    return StorPortInitialize(driverObject, registryPath, &initData, NULL);
}

static VOID AerovblkParseBarAddrs(_In_reads_bytes_(cfgLen) const UCHAR *cfgSpace,
                                 _In_ ULONG cfgLen,
                                 _Out_writes_(VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT) UINT64 barAddrs[6])
{
    ULONG i;

    UNREFERENCED_PARAMETER(cfgLen);

    RtlZeroMemory(barAddrs, sizeof(UINT64) * VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT);

    for (i = 0; i < PCI_TYPE0_ADDRESSES; i++) {
        ULONG val;
        ULONG barOff = 0x10u + (i * sizeof(ULONG));
        ULONG memType;

        RtlCopyMemory(&val, cfgSpace + barOff, sizeof(val));

        if (val == 0) {
            continue;
        }

        if ((val & 0x1u) != 0) {
            /* I/O BAR (not expected for contract v1). */
            continue;
        }

        memType = (val >> 1) & 0x3u;
        if (memType == 0x2u) {
            /* 64-bit BAR consumes this and the next BAR dword. */
            ULONG high;
            UINT64 base;
            if (i + 1 >= PCI_TYPE0_ADDRESSES) {
                break;
            }
            RtlCopyMemory(&high, cfgSpace + barOff + sizeof(ULONG), sizeof(high));
            base = ((UINT64)high << 32) | (UINT64)(val & ~0xFu);
            barAddrs[i] = base;
            i++;
        } else {
            barAddrs[i] = (UINT64)(val & ~0xFu);
        }
    }
}

ULONG AerovblkHwFindAdapter(_In_ PVOID deviceExtension,
                           _In_ PVOID hwContext,
                           _In_ PVOID busInformation,
                           _In_ PCHAR argumentString,
                           _Inout_ PPORT_CONFIGURATION_INFORMATION configInfo,
                           _Out_ PBOOLEAN again)
{
    PAEROVBLK_DEVICE_EXTENSION devExt;
    PACCESS_RANGE range;
    PVOID base;
    UCHAR pciCfg[256];
    UINT64 barAddrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t capRes;
    UINT64 deviceFeatures;
    USHORT numQueues;
    USHORT qsz;
    USHORT notifyOff;
    ULONG alignment;
    ULONG maxPhysBreaks;
    ULONG maxTransfer;
    VIRTIO_BLK_CONFIG blkCfg;
    NTSTATUS st;

    UNREFERENCED_PARAMETER(hwContext);
    UNREFERENCED_PARAMETER(busInformation);
    UNREFERENCED_PARAMETER(argumentString);

    *again = FALSE;

    if (configInfo->NumberOfAccessRanges < 1) {
        return SP_RETURN_NOT_FOUND;
    }

    range = &configInfo->AccessRanges[0];
    if (!range->RangeInMemory) {
        /* Contract v1 is modern-only (MMIO), no legacy I/O port transport. */
        return SP_RETURN_NOT_FOUND;
    }

    if (range->RangeLength < AEROVBLK_BAR0_LENGTH_REQUIRED) {
        return SP_RETURN_NOT_FOUND;
    }

    devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    RtlZeroMemory(devExt, sizeof(*devExt));
    KeInitializeSpinLock(&devExt->CommonCfgLock);

    base = StorPortGetDeviceBase(devExt,
                                 configInfo->AdapterInterfaceType,
                                 configInfo->SystemIoBusNumber,
                                 range->RangeStart,
                                 range->RangeLength,
                                 FALSE /* InIoSpace */);
    if (base == NULL) {
        return SP_RETURN_NOT_FOUND;
    }

    devExt->Bar0Va = base;
    devExt->Bar0Length = range->RangeLength;

    RtlZeroMemory(pciCfg, sizeof(pciCfg));
    if (StorPortGetBusData(devExt,
                           PCIConfiguration,
                           configInfo->SystemIoBusNumber,
                           configInfo->SlotNumber,
                           pciCfg,
                           sizeof(pciCfg)) != sizeof(pciCfg)) {
        return SP_RETURN_NOT_FOUND;
    }

    if (pciCfg[0x08] != (UCHAR)AEROVBLK_PCI_REVISION_ID) {
        /* Revision ID encodes contract major version. */
        return SP_RETURN_NOT_FOUND;
    }

    AerovblkParseBarAddrs(pciCfg, sizeof(pciCfg), barAddrs);

    RtlZeroMemory(&caps, sizeof(caps));
    capRes = virtio_pci_cap_parse(pciCfg, sizeof(pciCfg), barAddrs, &caps);
    if (capRes != VIRTIO_PCI_CAP_PARSE_OK) {
        AEROVBLK_LOG("virtio_pci_cap_parse failed: %s", virtio_pci_cap_parse_result_str(capRes));
        return SP_RETURN_NOT_FOUND;
    }

    /* Enforce contract v1 fixed capability layout. */
    if (caps.notify_off_multiplier != AEROVBLK_NOTIFY_OFF_MULTIPLIER_REQUIRED) {
        return SP_RETURN_NOT_FOUND;
    }

    if (caps.common_cfg.bar != 0 || caps.common_cfg.offset != 0x0000u || caps.common_cfg.length != 0x0100u) {
        return SP_RETURN_NOT_FOUND;
    }
    if (caps.notify_cfg.bar != 0 || caps.notify_cfg.offset != 0x1000u || caps.notify_cfg.length != 0x0100u) {
        return SP_RETURN_NOT_FOUND;
    }
    if (caps.isr_cfg.bar != 0 || caps.isr_cfg.offset != 0x2000u || caps.isr_cfg.length != 0x0020u) {
        return SP_RETURN_NOT_FOUND;
    }
    if (caps.device_cfg.bar != 0 || caps.device_cfg.offset != 0x3000u || caps.device_cfg.length != 0x0100u) {
        return SP_RETURN_NOT_FOUND;
    }

    devExt->CommonCfg = (volatile virtio_pci_common_cfg *)((PUCHAR)base + caps.common_cfg.offset);
    devExt->NotifyBase = (volatile UCHAR *)((PUCHAR)base + caps.notify_cfg.offset);
    devExt->NotifyOffMultiplier = caps.notify_off_multiplier;
    devExt->NotifyLength = caps.notify_cfg.length;
    devExt->IsrStatus = (volatile UCHAR *)((PUCHAR)base + caps.isr_cfg.offset);
    devExt->DeviceCfg = (volatile UCHAR *)((PUCHAR)base + caps.device_cfg.offset);
    devExt->DeviceCfgLength = caps.device_cfg.length;

    numQueues = READ_REGISTER_USHORT((volatile USHORT *)&devExt->CommonCfg->num_queues);
    if (numQueues != 1) {
        return SP_RETURN_NOT_FOUND;
    }

    {
        KIRQL oldIrql;
        AerovblkCommonCfgLock(devExt, &oldIrql);
        qsz = AerovblkReadQueueSizeLocked(devExt, (USHORT)AEROVBLK_QUEUE_INDEX);
        notifyOff = AerovblkReadQueueNotifyOffLocked(devExt, (USHORT)AEROVBLK_QUEUE_INDEX);
        AerovblkCommonCfgUnlock(devExt, oldIrql);
    }

    if (qsz != (USHORT)AEROVBLK_QUEUE_SIZE) {
        return SP_RETURN_NOT_FOUND;
    }
    if (notifyOff != 0) {
        return SP_RETURN_NOT_FOUND;
    }

    devExt->QueueSize = qsz;
    if (!AerovblkComputeQueueNotifyAddr(devExt, notifyOff)) {
        return SP_RETURN_NOT_FOUND;
    }

    /* Enforce contract v1 feature bits. */
    deviceFeatures = AerovblkReadDeviceFeatures(devExt);
    if ((deviceFeatures & (VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_BLK_F_SEG_MAX | VIRTIO_BLK_F_BLK_SIZE | VIRTIO_BLK_F_FLUSH)) !=
        (VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_BLK_F_SEG_MAX | VIRTIO_BLK_F_BLK_SIZE | VIRTIO_BLK_F_FLUSH)) {
        return SP_RETURN_NOT_FOUND;
    }
    if ((deviceFeatures & VIRTIO_F_RING_EVENT_IDX) != 0) {
        return SP_RETURN_NOT_FOUND;
    }

    RtlZeroMemory(&blkCfg, sizeof(blkCfg));
    st = AerovblkReadDeviceConfig(devExt, 0, &blkCfg, sizeof(blkCfg));
    if (!NT_SUCCESS(st)) {
        return SP_RETURN_NOT_FOUND;
    }

    /* Contract v1: size_max is not used and must be 0. */
    if (blkCfg.SizeMax != 0) {
        return SP_RETURN_NOT_FOUND;
    }

    if (blkCfg.SegMax == 0) {
        return SP_RETURN_NOT_FOUND;
    }

    /* Configure Storport properties (SCSI adapter with a single LU). */
    configInfo->NumberOfBuses = 1;
    configInfo->MaximumNumberOfTargets = 1;
    configInfo->MaximumNumberOfLogicalUnits = 1;
    configInfo->ScatterGather = TRUE;
    configInfo->Master = TRUE;
    configInfo->CachesData = FALSE;

    alignment = AEROVBLK_LOGICAL_SECTOR_SIZE;
    if (blkCfg.BlkSize >= AEROVBLK_LOGICAL_SECTOR_SIZE && (blkCfg.BlkSize % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0 &&
        ((blkCfg.BlkSize & (blkCfg.BlkSize - 1)) == 0)) {
        alignment = blkCfg.BlkSize;
    }

    maxPhysBreaks = blkCfg.SegMax;
    if (maxPhysBreaks > AEROVBLK_MAX_DATA_SG) {
        maxPhysBreaks = AEROVBLK_MAX_DATA_SG;
    }
    if (maxPhysBreaks == 0) {
        maxPhysBreaks = 1;
    }

    /*
     * Bound maximum transfer by worst-case SG fragmentation (one segment per page).
     * This keeps Storport from issuing SRBs that exceed the device's seg_max.
     */
    maxTransfer = maxPhysBreaks * PAGE_SIZE;
    if (maxTransfer > (1024u * 1024u)) {
        maxTransfer = 1024u * 1024u;
    }
    maxTransfer -= (maxTransfer % AEROVBLK_LOGICAL_SECTOR_SIZE);
    if (maxTransfer == 0) {
        maxTransfer = AEROVBLK_LOGICAL_SECTOR_SIZE;
    }

    configInfo->AlignmentMask = alignment - 1;
    configInfo->MaximumTransferLength = maxTransfer;
    configInfo->NumberOfPhysicalBreaks = maxPhysBreaks;

    /* Initialize runtime state. */
    devExt->LogicalSectorSize = alignment;
    devExt->CapacitySectors = 0;
    devExt->Removed = FALSE;
    RtlZeroMemory(&devExt->LastSense, sizeof(devExt->LastSense));

    return SP_RETURN_FOUND;
}

BOOLEAN AerovblkHwInitialize(_In_ PVOID deviceExtension)
{
    PAEROVBLK_DEVICE_EXTENSION devExt;

    devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    return AerovblkDeviceBringUp(devExt, TRUE);
}

BOOLEAN AerovblkHwResetBus(_In_ PVOID deviceExtension, _In_ ULONG pathId)
{
    PAEROVBLK_DEVICE_EXTENSION devExt;

    UNREFERENCED_PARAMETER(pathId);

    devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    return AerovblkDeviceBringUp(devExt, FALSE);
}

SCSI_ADAPTER_CONTROL_STATUS AerovblkHwAdapterControl(_In_ PVOID deviceExtension,
                                                     _In_ SCSI_ADAPTER_CONTROL_TYPE controlType,
                                                     _In_ PVOID parameters)
{
    PAEROVBLK_DEVICE_EXTENSION devExt;

    devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

    switch (controlType) {
    case ScsiQuerySupportedControlTypes: {
        PSCSI_SUPPORTED_CONTROL_TYPE_LIST list;
        ULONG i;

        list = (PSCSI_SUPPORTED_CONTROL_TYPE_LIST)parameters;
        for (i = 0; i < list->MaxControlType; ++i) {
            list->SupportedTypeList[i] = FALSE;
        }

        list->SupportedTypeList[ScsiQuerySupportedControlTypes] = TRUE;
        list->SupportedTypeList[ScsiStopAdapter] = TRUE;
        list->SupportedTypeList[ScsiRestartAdapter] = TRUE;
        list->SupportedTypeList[ScsiRemoveAdapter] = TRUE;
        return ScsiAdapterControlSuccess;
    }

    case ScsiStopAdapter:
    case ScsiRemoveAdapter: {
        STOR_LOCK_HANDLE lock;

        devExt->Removed = TRUE;

        /* Stop the device (disables queues and clears pending interrupts). */
        AerovblkResetDevice(devExt);

        StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
        AerovblkAbortOutstandingRequestsLocked(devExt);
        if (devExt->Vq != NULL) {
            VirtqSplitReset(devExt->Vq);
        }
        StorPortReleaseSpinLock(devExt, &lock);
        return ScsiAdapterControlSuccess;
    }

    case ScsiRestartAdapter:
        devExt->Removed = FALSE;
        return AerovblkDeviceBringUp(devExt, FALSE) ? ScsiAdapterControlSuccess : ScsiAdapterControlUnsuccessful;

    default:
        return ScsiAdapterControlUnsuccessful;
    }
}

BOOLEAN AerovblkHwInterrupt(_In_ PVOID deviceExtension)
{
    PAEROVBLK_DEVICE_EXTENSION devExt;
    UCHAR isr;
    STOR_LOCK_HANDLE lock;
    void *cookie;
    UINT32 usedLen;
    PAEROVBLK_REQUEST_CONTEXT ctx;
    PSCSI_REQUEST_BLOCK srb;
    UCHAR statusByte;

    devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    if (devExt->IsrStatus == NULL || devExt->Vq == NULL) {
        return FALSE;
    }

    isr = READ_REGISTER_UCHAR((volatile UCHAR *)devExt->IsrStatus);
    if (isr == 0) {
        return FALSE;
    }

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

    while (VirtqSplitHasUsed(devExt->Vq)) {
        if (!NT_SUCCESS(VirtqSplitGetUsed(devExt->Vq, &cookie, &usedLen))) {
            break;
        }

        UNREFERENCED_PARAMETER(usedLen);

        ctx = (PAEROVBLK_REQUEST_CONTEXT)cookie;
        if (ctx == NULL) {
            continue;
        }

        srb = ctx->Srb;
        ctx->Srb = NULL;

        InsertTailList(&devExt->FreeRequestList, &ctx->Link);
        devExt->FreeRequestCount++;

        if (srb == NULL) {
            continue;
        }

        statusByte = *ctx->StatusByte;
        if (statusByte == VIRTIO_BLK_S_OK) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            continue;
        }

        if (statusByte == VIRTIO_BLK_S_UNSUPP) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x20, 0x00);
        } else {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_MEDIUM_ERROR, ctx->IsWrite ? 0x0C : 0x11, 0x00);
        }

        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    }

    StorPortReleaseSpinLock(devExt, &lock);
    StorPortNotification(NextRequest, devExt, NULL);
    return TRUE;
}

BOOLEAN AerovblkHwStartIo(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    PAEROVBLK_DEVICE_EXTENSION devExt;
    UCHAR op;

    devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

    if (srb->PathId != 0 || srb->TargetId != 0 || srb->Lun != 0) {
        AerovblkHandleUnsupported(devExt, srb);
        return TRUE;
    }

    if (devExt->Removed) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_NOT_READY, 0x04, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
        return TRUE;
    }

    if (srb->Function == SRB_FUNCTION_IO_CONTROL) {
        AerovblkHandleIoControl(devExt, srb);
        return TRUE;
    }

    if (srb->Function != SRB_FUNCTION_EXECUTE_SCSI) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return TRUE;
    }

    op = srb->Cdb[0];

    switch (op) {
    case SCSIOP_INQUIRY:
        AerovblkHandleInquiry(devExt, srb);
        return TRUE;

    case SCSIOP_TEST_UNIT_READY:
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return TRUE;

    case SCSIOP_REQUEST_SENSE:
        AerovblkHandleRequestSense(devExt, srb);
        return TRUE;

    case SCSIOP_READ_CAPACITY:
        AerovblkHandleReadCapacity10(devExt, srb);
        return TRUE;

    case SCSIOP_SERVICE_ACTION_IN16:
        if ((srb->Cdb[1] & 0x1F) == 0x10) {
            AerovblkHandleReadCapacity16(devExt, srb);
            return TRUE;
        }
        break;

    case SCSIOP_MODE_SENSE:
        AerovblkHandleModeSense(devExt, srb, FALSE);
        return TRUE;

    case SCSIOP_MODE_SENSE10:
        AerovblkHandleModeSense(devExt, srb, TRUE);
        return TRUE;

    case SCSIOP_VERIFY:
    case SCSIOP_VERIFY16:
    case SCSIOP_START_STOP_UNIT:
    case SCSIOP_MEDIUM_REMOVAL:
    case SCSIOP_RESERVE_UNIT:
    case SCSIOP_RELEASE_UNIT:
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return TRUE;

    case SCSIOP_SYNCHRONIZE_CACHE:
    case SCSIOP_SYNCHRONIZE_CACHE16:
        if (!devExt->SupportsFlush) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return TRUE;
        }
        return AerovblkQueueRequest(devExt, srb, VIRTIO_BLK_T_FLUSH, 0, NULL, FALSE);

    case SCSIOP_READ:
    case SCSIOP_WRITE: {
        ULONGLONG scsiLba;
        ULONG blocks;
        ULONG sectorsPerBlock;
        ULONGLONG sectorsLen;
        ULONGLONG virtioSector;
        ULONGLONG bytes64;
        PSTOR_SCATTER_GATHER_LIST sg;
        BOOLEAN isWrite;
        ULONG reqType;

        scsiLba = (ULONGLONG)AerovblkBe32ToCpu(&srb->Cdb[2]);
        blocks = (ULONG)AerovblkBe16ToCpu(&srb->Cdb[7]);
        if (blocks == 0) {
            blocks = 65536;
        }

        sectorsPerBlock = AerovblkSectorsPerLogicalBlock(devExt);
        virtioSector = scsiLba * (ULONGLONG)sectorsPerBlock;
        sectorsLen = (ULONGLONG)blocks * (ULONGLONG)sectorsPerBlock;
        bytes64 = (ULONGLONG)blocks * (ULONGLONG)devExt->LogicalSectorSize;

        if (sectorsPerBlock == 0 || virtioSector / sectorsPerBlock != scsiLba || sectorsLen / sectorsPerBlock != blocks ||
            virtioSector + sectorsLen < virtioSector) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        /* Contract v1 requires lengths in multiples of 512 bytes. */
        if (bytes64 == 0 || (bytes64 % AEROVBLK_LOGICAL_SECTOR_SIZE) != 0 || bytes64 > 0xFFFFFFFFull ||
            srb->DataTransferLength < (ULONG)bytes64) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if (devExt->CapacitySectors != 0 && virtioSector + sectorsLen > devExt->CapacitySectors) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x21, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        sg = StorPortGetScatterGatherList(devExt, srb);
        if (sg == NULL) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
            return TRUE;
        }

        isWrite = (op == SCSIOP_WRITE) ? TRUE : FALSE;
        reqType = isWrite ? VIRTIO_BLK_T_OUT : VIRTIO_BLK_T_IN;
        return AerovblkQueueRequest(devExt, srb, reqType, virtioSector, sg, isWrite);
    }

    case SCSIOP_READ16:
    case SCSIOP_WRITE16: {
        ULONGLONG scsiLba;
        ULONG blocks;
        ULONG sectorsPerBlock;
        ULONGLONG sectorsLen;
        ULONGLONG virtioSector;
        ULONGLONG bytes64;
        PSTOR_SCATTER_GATHER_LIST sg;
        BOOLEAN isWrite;
        ULONG reqType;

        scsiLba = AerovblkBe64ToCpu(&srb->Cdb[2]);
        blocks = AerovblkBe32ToCpu(&srb->Cdb[10]);
        if (blocks == 0) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return TRUE;
        }

        sectorsPerBlock = AerovblkSectorsPerLogicalBlock(devExt);
        virtioSector = scsiLba * (ULONGLONG)sectorsPerBlock;
        sectorsLen = (ULONGLONG)blocks * (ULONGLONG)sectorsPerBlock;
        bytes64 = (ULONGLONG)blocks * (ULONGLONG)devExt->LogicalSectorSize;

        if (sectorsPerBlock == 0 || virtioSector / sectorsPerBlock != scsiLba || sectorsLen / sectorsPerBlock != blocks ||
            virtioSector + sectorsLen < virtioSector) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if ((bytes64 % AEROVBLK_LOGICAL_SECTOR_SIZE) != 0 || bytes64 > 0xFFFFFFFFull || srb->DataTransferLength < (ULONG)bytes64) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if (devExt->CapacitySectors != 0 && virtioSector + sectorsLen > devExt->CapacitySectors) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x21, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        sg = StorPortGetScatterGatherList(devExt, srb);
        if (sg == NULL) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
            return TRUE;
        }

        isWrite = (op == SCSIOP_WRITE16) ? TRUE : FALSE;
        reqType = isWrite ? VIRTIO_BLK_T_OUT : VIRTIO_BLK_T_IN;
        return AerovblkQueueRequest(devExt, srb, reqType, virtioSector, sg, isWrite);
    }

    default:
        break;
    }

    AerovblkHandleUnsupported(devExt, srb);
    return TRUE;
}
