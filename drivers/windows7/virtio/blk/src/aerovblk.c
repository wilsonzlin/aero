#include "../include/aerovblk.h"

static VOID AerovblkSetSense(
    _Inout_ PAEROVBLK_DEVICE_EXTENSION devExt,
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ UCHAR senseKey,
    _In_ UCHAR asc,
    _In_ UCHAR ascq)
{
    SENSE_DATA sense;
    RtlZeroMemory(&sense, sizeof(sense));
    sense.ErrorCode = 0x70;
    sense.SenseKey = senseKey;
    sense.AdditionalSenseCode = asc;
    sense.AdditionalSenseCodeQualifier = ascq;
    sense.AdditionalSenseLength = sizeof(SENSE_DATA) - FIELD_OFFSET(SENSE_DATA, CommandSpecificInformation);

    devExt->LastSense = sense;

    if (srb->SenseInfoBuffer != NULL && srb->SenseInfoBufferLength != 0) {
        const ULONG copyLen = (srb->SenseInfoBufferLength < sizeof(sense)) ? srb->SenseInfoBufferLength : sizeof(sense);
        RtlCopyMemory(srb->SenseInfoBuffer, &sense, copyLen);
        srb->SrbStatus |= SRB_STATUS_AUTOSENSE_VALID;
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

static __forceinline ULONGLONG AerovblkBe64ToCpu(_In_reads_bytes_(8) const UCHAR* p)
{
    return ((ULONGLONG)p[0] << 56) | ((ULONGLONG)p[1] << 48) | ((ULONGLONG)p[2] << 40) | ((ULONGLONG)p[3] << 32) |
           ((ULONGLONG)p[4] << 24) | ((ULONGLONG)p[5] << 16) | ((ULONGLONG)p[6] << 8) | ((ULONGLONG)p[7]);
}

static __forceinline ULONG AerovblkBe32ToCpu(_In_reads_bytes_(4) const UCHAR* p)
{
    return ((ULONG)p[0] << 24) | ((ULONG)p[1] << 16) | ((ULONG)p[2] << 8) | (ULONG)p[3];
}

static __forceinline USHORT AerovblkBe16ToCpu(_In_reads_bytes_(2) const UCHAR* p)
{
    return (USHORT)(((USHORT)p[0] << 8) | (USHORT)p[1]);
}

static VOID AerovblkWriteBe32(_Out_writes_bytes_(4) UCHAR* p, _In_ ULONG v)
{
    p[0] = (UCHAR)(v >> 24);
    p[1] = (UCHAR)(v >> 16);
    p[2] = (UCHAR)(v >> 8);
    p[3] = (UCHAR)v;
}

static VOID AerovblkWriteBe64(_Out_writes_bytes_(8) UCHAR* p, _In_ ULONGLONG v)
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
    if (devExt->LogicalSectorSize == 0) {
        return 0;
    }
    const ULONGLONG capBytes = devExt->CapacitySectors * (ULONGLONG)AEROVBLK_LOGICAL_SECTOR_SIZE;
    return capBytes / (ULONGLONG)devExt->LogicalSectorSize;
}

static VOID AerovblkResetQueueStateLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    if (devExt->Vq.RingVa == NULL || devExt->Vq.FreeStack == NULL || devExt->Vq.QueueSize == 0) {
        return;
    }

    RtlZeroMemory(devExt->Vq.RingVa, devExt->Vq.RingBytes);
    devExt->Vq.AvailIdxShadow = 0;
    devExt->Vq.LastUsedIdx = 0;

    for (USHORT i = 0; i < devExt->Vq.QueueSize; ++i) {
        devExt->Vq.FreeStack[i] = (USHORT)(devExt->Vq.QueueSize - 1 - i);
    }
    devExt->Vq.FreeCount = devExt->Vq.QueueSize;
}

static VOID AerovblkAbortOutstandingRequestsLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt)
{
    if (devExt->RequestContexts == NULL || devExt->Vq.QueueSize == 0) {
        return;
    }

    for (USHORT i = 0; i < devExt->Vq.QueueSize; ++i) {
        PAEROVBLK_REQUEST_CONTEXT ctx = &devExt->RequestContexts[i];
        PSCSI_REQUEST_BLOCK srb = ctx->Srb;
        ctx->Srb = NULL;
        if (srb == NULL) {
            continue;
        }

        AerovblkSetSense(devExt, srb, SCSI_SENSE_ABORTED_COMMAND, 0x00, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    }
}

static BOOLEAN AerovblkDeviceBringUp(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ BOOLEAN allocateResources)
{
    if (!allocateResources) {
        STOR_LOCK_HANDLE lock;
        StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
        AerovblkAbortOutstandingRequestsLocked(devExt);
        AerovblkResetQueueStateLocked(devExt);
        StorPortReleaseSpinLock(devExt, &lock);
    }

    AerovirtioPciLegacyReset(&devExt->Pci);

    UCHAR status = AEROVIRTIO_STATUS_ACKNOWLEDGE;
    AerovirtioPciLegacySetStatus(&devExt->Pci, status);
    status |= AEROVIRTIO_STATUS_DRIVER;
    AerovirtioPciLegacySetStatus(&devExt->Pci, status);

    const ULONG hostFeatures = AerovirtioPciLegacyReadHostFeatures(&devExt->Pci);
    const ULONG wanted = AEROVIRTIO_RING_F_INDIRECT_DESC | AEROVIRTIO_BLK_F_FLUSH | AEROVIRTIO_BLK_F_BLK_SIZE;
    devExt->NegotiatedFeatures = hostFeatures & wanted;
    devExt->SupportsIndirect = (devExt->NegotiatedFeatures & AEROVIRTIO_RING_F_INDIRECT_DESC) != 0;
    devExt->SupportsFlush = (devExt->NegotiatedFeatures & AEROVIRTIO_BLK_F_FLUSH) != 0;

    AerovirtioPciLegacyWriteGuestFeatures(&devExt->Pci, devExt->NegotiatedFeatures);

    status |= AEROVIRTIO_STATUS_FEATURES_OK;
    AerovirtioPciLegacySetStatus(&devExt->Pci, status);

    status = AerovirtioPciLegacyGetStatus(&devExt->Pci);
    if ((status & AEROVIRTIO_STATUS_FEATURES_OK) == 0) {
        AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
        return FALSE;
    }

    AerovirtioPciLegacySelectQueue(&devExt->Pci, 0);
    const USHORT queueSize = AerovirtioPciLegacyReadQueueSize(&devExt->Pci);
    if (queueSize == 0) {
        AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
        return FALSE;
    }

    if (allocateResources) {
        const ULONG ringBytes = AerovirtqGetRingBytes(queueSize);
        PHYSICAL_ADDRESS low;
        PHYSICAL_ADDRESS high;
        PHYSICAL_ADDRESS boundary;
        low.QuadPart = 0;
        high.QuadPart = 0xFFFFFFFFull;
        boundary.QuadPart = 0;

        PVOID ringVa = StorPortAllocateContiguousMemorySpecifyCache(
            devExt,
            ringBytes,
            low,
            high,
            boundary,
            MmNonCached);
        if (ringVa == NULL) {
            AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
            return FALSE;
        }

        ULONG paLen = ringBytes;
        const STOR_PHYSICAL_ADDRESS ringPa = StorPortGetPhysicalAddress(devExt, NULL, ringVa, &paLen);
        if (paLen < ringBytes) {
            AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
            return FALSE;
        }

        if (!AerovirtqInit(devExt, &devExt->Vq, 0, queueSize, ringVa, ringPa, ringBytes)) {
            AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
            return FALSE;
        }

        devExt->RequestContexts =
            (PAEROVBLK_REQUEST_CONTEXT)StorPortAllocatePool(devExt, sizeof(AEROVBLK_REQUEST_CONTEXT) * (ULONG)queueSize, 'bVrA');
        if (devExt->RequestContexts == NULL) {
            AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
            return FALSE;
        }
        RtlZeroMemory(devExt->RequestContexts, sizeof(AEROVBLK_REQUEST_CONTEXT) * (ULONG)queueSize);

        for (USHORT i = 0; i < queueSize; ++i) {
            PHYSICAL_ADDRESS rlow;
            PHYSICAL_ADDRESS rhigh;
            PHYSICAL_ADDRESS rboundary;
            rlow.QuadPart = 0;
            rhigh.QuadPart = 0xFFFFFFFFull;
            rboundary.QuadPart = 0;

            PVOID pageVa = StorPortAllocateContiguousMemorySpecifyCache(devExt, PAGE_SIZE, rlow, rhigh, rboundary, MmNonCached);
            if (pageVa == NULL) {
                AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
                return FALSE;
            }

            ULONG pageLen = PAGE_SIZE;
            const STOR_PHYSICAL_ADDRESS pagePa = StorPortGetPhysicalAddress(devExt, NULL, pageVa, &pageLen);
            if (pageLen < PAGE_SIZE) {
                AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
                return FALSE;
            }

            PAEROVBLK_REQUEST_CONTEXT ctx = &devExt->RequestContexts[i];
            ctx->SharedPageVa = pageVa;
            ctx->SharedPagePa = pagePa;
            ctx->ReqHdr = (volatile AEROVIRTIO_BLK_REQ*)((PUCHAR)pageVa + AEROVBLK_REQ_HDR_OFFSET);
            ctx->StatusByte = (volatile UCHAR*)((PUCHAR)pageVa + AEROVBLK_REQ_STATUS_OFFSET);
            ctx->IndirectDesc = (volatile AEROVIRTQ_DESC*)((PUCHAR)pageVa + AEROVBLK_REQ_INDIRECT_OFFSET);
            ctx->Srb = NULL;
            ctx->ScsiOp = 0;
            ctx->IsWrite = FALSE;
        }
    } else {
        if (devExt->Vq.QueueSize != queueSize) {
            AerovirtioPciLegacySetStatus(&devExt->Pci, (UCHAR)(status | AEROVIRTIO_STATUS_FAILED));
            return FALSE;
        }
    }

    const ULONG queuePfn = (ULONG)(devExt->Vq.RingPa.QuadPart >> PAGE_SHIFT);
    AerovirtioPciLegacyWriteQueuePfn(&devExt->Pci, queuePfn);

    AEROVIRTIO_BLK_CONFIG cfg;
    RtlZeroMemory(&cfg, sizeof(cfg));
    AerovirtioPciLegacyReadDeviceConfig(&devExt->Pci, 0, &cfg, sizeof(cfg));
    devExt->CapacitySectors = cfg.capacity;
    devExt->LogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
    if ((devExt->NegotiatedFeatures & AEROVIRTIO_BLK_F_BLK_SIZE) != 0 && cfg.blk_size >= AEROVBLK_LOGICAL_SECTOR_SIZE &&
        (cfg.blk_size % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0) {
        devExt->LogicalSectorSize = cfg.blk_size;
    }

    status |= AEROVIRTIO_STATUS_DRIVER_OK;
    AerovirtioPciLegacySetStatus(&devExt->Pci, status);
    StorPortNotification(NextRequest, devExt, NULL);
    return TRUE;
}

static BOOLEAN AerovblkQueueRequest(
    _Inout_ PAEROVBLK_DEVICE_EXTENSION devExt,
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ ULONG reqType,
    _In_ ULONGLONG startSector,
    _In_opt_ PSTOR_SCATTER_GATHER_LIST sg,
    _In_ BOOLEAN isWrite)
{
    STOR_LOCK_HANDLE lock;
    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

    if (devExt->Removed) {
        StorPortReleaseSpinLock(devExt, &lock);
        AerovblkSetSense(devExt, srb, SCSI_SENSE_NOT_READY, 0x04, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
        return TRUE;
    }

    const ULONG sgCount = (sg == NULL) ? 0 : sg->NumberOfElements;
    const ULONG chainCount = sgCount + 2;

    if (devExt->SupportsIndirect) {
        if (chainCount > AEROVBLK_MAX_INDIRECT_DESCS) {
            StorPortReleaseSpinLock(devExt, &lock);
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if (devExt->Vq.FreeCount == 0) {
            StorPortReleaseSpinLock(devExt, &lock);
            return FALSE;
        }

        const USHORT head = AerovirtqAllocDesc(&devExt->Vq);
        if (head == 0xFFFF) {
            StorPortReleaseSpinLock(devExt, &lock);
            return FALSE;
        }

        PAEROVBLK_REQUEST_CONTEXT ctx = &devExt->RequestContexts[head];
        ctx->Srb = srb;
        ctx->ScsiOp = srb->Cdb[0];
        ctx->IsWrite = isWrite;

        ctx->ReqHdr->type = reqType;
        ctx->ReqHdr->reserved = 0;
        ctx->ReqHdr->sector = startSector;
        *ctx->StatusByte = 0xFF;

        volatile AEROVIRTQ_DESC* ind = ctx->IndirectDesc;

        ind[0].addr = ctx->SharedPagePa.QuadPart + AEROVBLK_REQ_HDR_OFFSET;
        ind[0].len = sizeof(AEROVIRTIO_BLK_REQ);
        ind[0].flags = AEROVIRTQ_DESC_F_NEXT;
        ind[0].next = 1;

        for (ULONG i = 0; i < sgCount; ++i) {
            ind[1 + i].addr = sg->Elements[i].PhysicalAddress.QuadPart;
            ind[1 + i].len = sg->Elements[i].Length;
            ind[1 + i].flags = (USHORT)(AEROVIRTQ_DESC_F_NEXT | (isWrite ? 0 : AEROVIRTQ_DESC_F_WRITE));
            ind[1 + i].next = (USHORT)(2 + i);
        }

        const USHORT statusIdx = (USHORT)(1 + sgCount);
        if (sgCount != 0) {
            ind[statusIdx - 1].next = statusIdx;
        } else {
            ind[0].next = statusIdx;
        }

        ind[statusIdx].addr = ctx->SharedPagePa.QuadPart + AEROVBLK_REQ_STATUS_OFFSET;
        ind[statusIdx].len = 1;
        ind[statusIdx].flags = AEROVIRTQ_DESC_F_WRITE;
        ind[statusIdx].next = 0;

        devExt->Vq.Desc[head].addr = ctx->SharedPagePa.QuadPart + AEROVBLK_REQ_INDIRECT_OFFSET;
        devExt->Vq.Desc[head].len = chainCount * sizeof(AEROVIRTQ_DESC);
        devExt->Vq.Desc[head].flags = AEROVIRTQ_DESC_F_INDIRECT;
        devExt->Vq.Desc[head].next = 0;

        AerovirtqSubmit(&devExt->Vq, head);
        AerovirtioPciLegacyNotifyQueue(&devExt->Pci, 0);

        StorPortReleaseSpinLock(devExt, &lock);
        StorPortNotification(NextRequest, devExt, NULL);
        return TRUE;
    }

    if (chainCount > devExt->Vq.QueueSize) {
        StorPortReleaseSpinLock(devExt, &lock);
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return TRUE;
    }

    if (devExt->Vq.FreeCount < chainCount) {
        StorPortReleaseSpinLock(devExt, &lock);
        return FALSE;
    }

    USHORT descIdx[AEROVBLK_MAX_INDIRECT_DESCS];
    for (ULONG i = 0; i < chainCount; ++i) {
        const USHORT didx = AerovirtqAllocDesc(&devExt->Vq);
        if (didx == 0xFFFF) {
            for (ULONG j = 0; j < i; ++j) {
                AerovirtqFreeDesc(&devExt->Vq, descIdx[j]);
            }
            StorPortReleaseSpinLock(devExt, &lock);
            return FALSE;
        }
        descIdx[i] = didx;
    }

    const USHORT head = descIdx[0];
    PAEROVBLK_REQUEST_CONTEXT ctx = &devExt->RequestContexts[head];
    ctx->Srb = srb;
    ctx->ScsiOp = srb->Cdb[0];
    ctx->IsWrite = isWrite;

    ctx->ReqHdr->type = reqType;
    ctx->ReqHdr->reserved = 0;
    ctx->ReqHdr->sector = startSector;
    *ctx->StatusByte = 0xFF;

    devExt->Vq.Desc[head].addr = ctx->SharedPagePa.QuadPart + AEROVBLK_REQ_HDR_OFFSET;
    devExt->Vq.Desc[head].len = sizeof(AEROVIRTIO_BLK_REQ);
    devExt->Vq.Desc[head].flags = AEROVIRTQ_DESC_F_NEXT;
    devExt->Vq.Desc[head].next = descIdx[1];

    for (ULONG i = 0; i < sgCount; ++i) {
        const USHORT didx = descIdx[1 + i];
        devExt->Vq.Desc[didx].addr = sg->Elements[i].PhysicalAddress.QuadPart;
        devExt->Vq.Desc[didx].len = sg->Elements[i].Length;
        devExt->Vq.Desc[didx].flags = isWrite ? 0 : AEROVIRTQ_DESC_F_WRITE;

        const BOOLEAN hasNext = (i != sgCount - 1);
        if (hasNext) {
            devExt->Vq.Desc[didx].flags |= AEROVIRTQ_DESC_F_NEXT;
            devExt->Vq.Desc[didx].next = descIdx[1 + i + 1];
        } else {
            devExt->Vq.Desc[didx].flags |= AEROVIRTQ_DESC_F_NEXT;
            devExt->Vq.Desc[didx].next = descIdx[1 + sgCount];
        }
    }

    const USHORT statusDesc = descIdx[1 + sgCount];
    if (sgCount == 0) {
        devExt->Vq.Desc[head].next = statusDesc;
    }

    devExt->Vq.Desc[statusDesc].addr = ctx->SharedPagePa.QuadPart + AEROVBLK_REQ_STATUS_OFFSET;
    devExt->Vq.Desc[statusDesc].len = 1;
    devExt->Vq.Desc[statusDesc].flags = AEROVIRTQ_DESC_F_WRITE;
    devExt->Vq.Desc[statusDesc].next = 0;

    if (sgCount != 0) {
        const USHORT lastDataDesc = descIdx[sgCount];
        devExt->Vq.Desc[lastDataDesc].next = statusDesc;
    }

    AerovirtqSubmit(&devExt->Vq, head);
    AerovirtioPciLegacyNotifyQueue(&devExt->Pci, 0);

    StorPortReleaseSpinLock(devExt, &lock);
    StorPortNotification(NextRequest, devExt, NULL);
    return TRUE;
}

static VOID AerovblkHandleInquiry(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    const ULONG allocLen = srb->Cdb[4];
    const UCHAR evpd = (srb->Cdb[1] & 0x01) ? 1 : 0;
    const UCHAR pageCode = srb->Cdb[2];

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    PUCHAR out = (PUCHAR)srb->DataBuffer;
    const ULONG outLen = (srb->DataTransferLength < allocLen) ? srb->DataTransferLength : allocLen;
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
            const ULONG copy = (outLen - 4 < sizeof(pages)) ? (outLen - 4) : sizeof(pages);
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
            const ULONG serialLen = sizeof(serial) - 1;
            const ULONG copy = (outLen - 4 < serialLen) ? (outLen - 4) : serialLen;
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

    INQUIRYDATA inq;
    RtlZeroMemory(&inq, sizeof(inq));
    inq.DeviceType = DIRECT_ACCESS_DEVICE;
    inq.Versions = 5;
    inq.ResponseDataFormat = 2;
    inq.AdditionalLength = sizeof(INQUIRYDATA) - 5;
    RtlCopyMemory(inq.VendorId, "AERO    ", 8);
    RtlCopyMemory(inq.ProductId, "VIRTIO-BLK      ", 16);
    RtlCopyMemory(inq.ProductRevisionLevel, "0001", 4);

    const ULONG copy = (outLen < sizeof(inq)) ? outLen : sizeof(inq);
    RtlCopyMemory(out, &inq, copy);
    srb->DataTransferLength = copy;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleReadCapacity10(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    if (srb->DataBuffer == NULL || srb->DataTransferLength < 8) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    PUCHAR out = (PUCHAR)srb->DataBuffer;
    RtlZeroMemory(out, 8);

    const ULONGLONG totalBlocks = AerovblkTotalLogicalBlocks(devExt);
    const ULONGLONG lastLba = (totalBlocks == 0) ? 0 : (totalBlocks - 1);

    const ULONG lastLba32 = (lastLba > 0xFFFFFFFFull) ? 0xFFFFFFFFu : (ULONG)lastLba;
    AerovblkWriteBe32(out + 0, lastLba32);
    AerovblkWriteBe32(out + 4, devExt->LogicalSectorSize);
    srb->DataTransferLength = 8;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleReadCapacity16(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    const ULONG allocLen = AerovblkBe32ToCpu(&srb->Cdb[10]);

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    const ULONG outLen = (srb->DataTransferLength < allocLen) ? srb->DataTransferLength : allocLen;
    if (outLen < 12) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return;
    }

    PUCHAR out = (PUCHAR)srb->DataBuffer;
    RtlZeroMemory(out, outLen);

    const ULONGLONG totalBlocks = AerovblkTotalLogicalBlocks(devExt);
    const ULONGLONG lastLba = (totalBlocks == 0) ? 0 : (totalBlocks - 1);
    AerovblkWriteBe64(out + 0, lastLba);
    AerovblkWriteBe32(out + 8, devExt->LogicalSectorSize);
    srb->DataTransferLength = (outLen < 32) ? outLen : 32;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleModeSense(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ BOOLEAN mode10)
{
    const UCHAR pageCode = srb->Cdb[2] & 0x3F;
    const ULONG allocLen = mode10 ? AerovblkBe16ToCpu(&srb->Cdb[7]) : srb->Cdb[4];

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
        return;
    }

    PUCHAR out = (PUCHAR)srb->DataBuffer;
    const ULONG outLen = (srb->DataTransferLength < allocLen) ? srb->DataTransferLength : allocLen;
    RtlZeroMemory(out, outLen);

    UCHAR cachePage[20];
    RtlZeroMemory(cachePage, sizeof(cachePage));
    cachePage[0] = 0x08;
    cachePage[1] = 0x12;
    cachePage[2] = 0x00;
    cachePage[2] |= 0x04;

    ULONG payloadLen = 0;
    if (pageCode == 0x3F || pageCode == 0x08) {
        payloadLen = sizeof(cachePage);
    }

    if (mode10) {
        if (outLen < 8) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return;
        }

        const USHORT modeDataLen = (USHORT)(6 + payloadLen);
        out[0] = (UCHAR)(modeDataLen >> 8);
        out[1] = (UCHAR)modeDataLen;
        out[7] = 0;

        ULONG copy = payloadLen;
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
    out[3] = 0;

    ULONG copy = payloadLen;
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
    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
        return;
    }

    const ULONG copyLen = (srb->DataTransferLength < sizeof(devExt->LastSense)) ? srb->DataTransferLength : sizeof(devExt->LastSense);
    RtlCopyMemory(srb->DataBuffer, &devExt->LastSense, copyLen);
    srb->DataTransferLength = copyLen;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleIoControl(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    if (srb->DataBuffer == NULL || srb->DataTransferLength < sizeof(SRB_IO_CONTROL)) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
        return;
    }

    PSRB_IO_CONTROL ctrl = (PSRB_IO_CONTROL)srb->DataBuffer;
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

    PAEROVBLK_QUERY_INFO info = (PAEROVBLK_QUERY_INFO)((PUCHAR)srb->DataBuffer + sizeof(SRB_IO_CONTROL));
    info->NegotiatedFeatures = devExt->NegotiatedFeatures;
    info->QueueSize = devExt->Vq.QueueSize;
    info->FreeCount = devExt->Vq.FreeCount;
    info->AvailIdx = devExt->Vq.Avail->idx;
    info->UsedIdx = devExt->Vq.Used->idx;

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
    initData.NumberOfAccessRanges = 1;
    initData.TaggedQueuing = TRUE;
    initData.MultipleRequestPerLu = TRUE;
    initData.AutoRequestSense = FALSE;
    initData.NeedPhysicalAddresses = TRUE;
    initData.MapBuffers = TRUE;

    return StorPortInitialize(driverObject, registryPath, &initData, NULL);
}

ULONG AerovblkHwFindAdapter(
    _In_ PVOID deviceExtension,
    _In_ PVOID hwContext,
    _In_ PVOID busInformation,
    _In_ PCHAR argumentString,
    _Inout_ PPORT_CONFIGURATION_INFORMATION configInfo,
    _Out_ PBOOLEAN again)
{
    UNREFERENCED_PARAMETER(hwContext);
    UNREFERENCED_PARAMETER(busInformation);
    UNREFERENCED_PARAMETER(argumentString);

    *again = FALSE;

    if (configInfo->NumberOfAccessRanges < 1) {
        return SP_RETURN_NOT_FOUND;
    }

    PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    RtlZeroMemory(devExt, sizeof(*devExt));

    PACCESS_RANGE range = &configInfo->AccessRanges[0];
    const BOOLEAN inIoSpace = (range->RangeInMemory == FALSE);
    PVOID base = StorPortGetDeviceBase(
        devExt,
        configInfo->AdapterInterfaceType,
        configInfo->SystemIoBusNumber,
        range->RangeStart,
        range->RangeLength,
        inIoSpace);
    if (base == NULL) {
        return SP_RETURN_NOT_FOUND;
    }

    devExt->Pci.Base = (PUCHAR)base;
    devExt->Pci.Length = range->RangeLength;
    devExt->Pci.AccessType = inIoSpace ? AerovirtioPciAccessPort : AerovirtioPciAccessMemory;

    AerovirtioPciLegacySelectQueue(&devExt->Pci, 0);
    const USHORT hwQueueSize = AerovirtioPciLegacyReadQueueSize(&devExt->Pci);
    const ULONG hostFeatures = AerovirtioPciLegacyReadHostFeatures(&devExt->Pci);
    ULONG maxPhysBreaks = 17;
    if (hostFeatures & AEROVIRTIO_RING_F_INDIRECT_DESC) {
        maxPhysBreaks = (ULONG)(AEROVBLK_MAX_INDIRECT_DESCS - 2);
    } else if (hwQueueSize > 2) {
        maxPhysBreaks = (ULONG)(hwQueueSize - 2);
    }

    devExt->LogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
    devExt->CapacitySectors = 0;
    devExt->Removed = FALSE;
    RtlZeroMemory(&devExt->LastSense, sizeof(devExt->LastSense));

    configInfo->NumberOfBuses = 1;
    configInfo->MaximumNumberOfTargets = 1;
    configInfo->MaximumNumberOfLogicalUnits = 1;
    configInfo->ScatterGather = TRUE;
    configInfo->Master = TRUE;
    configInfo->CachesData = FALSE;
    configInfo->MaximumTransferLength = 1024 * 1024;
    configInfo->NumberOfPhysicalBreaks = maxPhysBreaks;

    return SP_RETURN_FOUND;
}

BOOLEAN AerovblkHwInitialize(_In_ PVOID deviceExtension)
{
    PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    return AerovblkDeviceBringUp(devExt, TRUE);
}

BOOLEAN AerovblkHwResetBus(_In_ PVOID deviceExtension, _In_ ULONG pathId)
{
    UNREFERENCED_PARAMETER(pathId);
    PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    return AerovblkDeviceBringUp(devExt, FALSE);
}

SCSI_ADAPTER_CONTROL_STATUS AerovblkHwAdapterControl(
    _In_ PVOID deviceExtension,
    _In_ SCSI_ADAPTER_CONTROL_TYPE controlType,
    _In_ PVOID parameters)
{
    PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

    switch (controlType) {
    case ScsiQuerySupportedControlTypes: {
        PSCSI_SUPPORTED_CONTROL_TYPE_LIST list = (PSCSI_SUPPORTED_CONTROL_TYPE_LIST)parameters;
        for (ULONG i = 0; i < list->MaxControlType; ++i) {
            list->SupportedTypeList[i] = FALSE;
        }
        list->SupportedTypeList[ScsiQuerySupportedControlTypes] = TRUE;
        list->SupportedTypeList[ScsiStopAdapter] = TRUE;
        list->SupportedTypeList[ScsiRestartAdapter] = TRUE;
        list->SupportedTypeList[ScsiRemoveAdapter] = TRUE;
        return ScsiAdapterControlSuccess;
    }
    case ScsiStopAdapter:
    case ScsiRemoveAdapter:
        devExt->Removed = TRUE;
        {
            STOR_LOCK_HANDLE lock;
            StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
            AerovblkAbortOutstandingRequestsLocked(devExt);
            AerovblkResetQueueStateLocked(devExt);
            StorPortReleaseSpinLock(devExt, &lock);
        }
        AerovirtioPciLegacyReset(&devExt->Pci);
        return ScsiAdapterControlSuccess;
    case ScsiRestartAdapter:
        devExt->Removed = FALSE;
        if (!AerovblkDeviceBringUp(devExt, FALSE)) {
            return ScsiAdapterControlUnsuccessful;
        }
        return ScsiAdapterControlSuccess;
    default:
        return ScsiAdapterControlUnsuccessful;
    }
}

BOOLEAN AerovblkHwInterrupt(_In_ PVOID deviceExtension)
{
    PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
    const UCHAR isr = AerovirtioPciLegacyReadIsr(&devExt->Pci);
    if (isr == 0) {
        return FALSE;
    }

    STOR_LOCK_HANDLE lock;
    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

    USHORT head;
    ULONG usedLen;
    while (AerovirtqPopUsed(&devExt->Vq, &head, &usedLen)) {
        UNREFERENCED_PARAMETER(usedLen);

        if (head >= devExt->Vq.QueueSize) {
            continue;
        }

        PAEROVBLK_REQUEST_CONTEXT ctx = &devExt->RequestContexts[head];
        PSCSI_REQUEST_BLOCK srb = ctx->Srb;
        ctx->Srb = NULL;

        const UCHAR statusByte = *ctx->StatusByte;

        AerovirtqFreeChain(&devExt->Vq, head);

        if (srb == NULL) {
            continue;
        }

        if (statusByte == AEROVIRTIO_BLK_S_OK) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            continue;
        }

        if (statusByte == AEROVIRTIO_BLK_S_UNSUPP) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x20, 0x00);
        } else {
            const UCHAR asc = ctx->IsWrite ? 0x0C : 0x11;
            AerovblkSetSense(devExt, srb, SCSI_SENSE_MEDIUM_ERROR, asc, 0x00);
        }
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    }

    StorPortReleaseSpinLock(devExt, &lock);

    StorPortNotification(NextRequest, devExt, NULL);
    return TRUE;
}

BOOLEAN AerovblkHwStartIo(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb)
{
    PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

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

    const UCHAR op = srb->Cdb[0];

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
        if (srb->Cdb[1] == 0x10) {
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
    case SCSIOP_SYNCHRONIZE_CACHE:
    case SCSIOP_SYNCHRONIZE_CACHE16:
        if (!devExt->SupportsFlush) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return TRUE;
        }
        return AerovblkQueueRequest(devExt, srb, AEROVIRTIO_BLK_T_FLUSH, 0, NULL, FALSE);
    case SCSIOP_READ:
    case SCSIOP_WRITE: {
        const ULONGLONG scsiLba = (ULONGLONG)AerovblkBe32ToCpu(&srb->Cdb[2]);
        ULONG blocks = (ULONG)AerovblkBe16ToCpu(&srb->Cdb[7]);
        if (blocks == 0) {
            blocks = 65536;
        }

        const ULONG sectorsPerBlock = AerovblkSectorsPerLogicalBlock(devExt);
        const ULONGLONG sectorsLen = (ULONGLONG)blocks * (ULONGLONG)sectorsPerBlock;
        const ULONGLONG virtioSector = scsiLba * (ULONGLONG)sectorsPerBlock;
        const ULONGLONG bytes64 = (ULONGLONG)blocks * (ULONGLONG)devExt->LogicalSectorSize;

        if (sectorsPerBlock == 0 || virtioSector / sectorsPerBlock != scsiLba || sectorsLen / sectorsPerBlock != blocks ||
            virtioSector + sectorsLen < virtioSector) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if (bytes64 == 0 || bytes64 > 0xFFFFFFFFull || srb->DataTransferLength < (ULONG)bytes64) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if (devExt->CapacitySectors != 0 && virtioSector + sectorsLen > devExt->CapacitySectors) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x21, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        PSTOR_SCATTER_GATHER_LIST sg = StorPortGetScatterGatherList(devExt, srb);
        if (sg == NULL) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
            return TRUE;
        }

        const BOOLEAN isWrite = (op == SCSIOP_WRITE);
        const ULONG reqType = isWrite ? AEROVIRTIO_BLK_T_OUT : AEROVIRTIO_BLK_T_IN;
        return AerovblkQueueRequest(devExt, srb, reqType, virtioSector, sg, isWrite);
    }
    case SCSIOP_READ16:
    case SCSIOP_WRITE16: {
        const ULONGLONG scsiLba = AerovblkBe64ToCpu(&srb->Cdb[2]);
        const ULONG blocks = AerovblkBe32ToCpu(&srb->Cdb[10]);
        if (blocks == 0) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
            return TRUE;
        }

        const ULONG sectorsPerBlock = AerovblkSectorsPerLogicalBlock(devExt);
        const ULONGLONG sectorsLen = (ULONGLONG)blocks * (ULONGLONG)sectorsPerBlock;
        const ULONGLONG virtioSector = scsiLba * (ULONGLONG)sectorsPerBlock;
        const ULONGLONG bytes64 = (ULONGLONG)blocks * (ULONGLONG)devExt->LogicalSectorSize;

        if (sectorsPerBlock == 0 || virtioSector / sectorsPerBlock != scsiLba || sectorsLen / sectorsPerBlock != blocks ||
            virtioSector + sectorsLen < virtioSector) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if (bytes64 > 0xFFFFFFFFull || srb->DataTransferLength < (ULONG)bytes64) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        if (devExt->CapacitySectors != 0 && virtioSector + sectorsLen > devExt->CapacitySectors) {
            AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x21, 0x00);
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
            return TRUE;
        }

        PSTOR_SCATTER_GATHER_LIST sg = StorPortGetScatterGatherList(devExt, srb);
        if (sg == NULL) {
            AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
            return TRUE;
        }

        const BOOLEAN isWrite = (op == SCSIOP_WRITE16);
        const ULONG reqType = isWrite ? AEROVIRTIO_BLK_T_OUT : AEROVIRTIO_BLK_T_IN;
        return AerovblkQueueRequest(devExt, srb, reqType, virtioSector, sg, isWrite);
    }
    default:
        break;
    }

    AerovblkHandleUnsupported(devExt, srb);
    return TRUE;
}
