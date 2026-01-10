#include "../include/aerovblk.h"

static VOID AerovblkCompleteSrb(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ UCHAR srbStatus);

static VOID AerovblkSetSense(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ UCHAR senseKey, _In_ UCHAR asc,
                             _In_ UCHAR ascq) {
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

static VOID AerovblkCompleteSrb(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ UCHAR srbStatus) {
  srb->SrbStatus = srbStatus;
  if ((srbStatus & SRB_STATUS_STATUS_MASK) == SRB_STATUS_SUCCESS) {
    srb->ScsiStatus = SCSISTAT_GOOD;
  }

  StorPortNotification(RequestComplete, deviceExtension, srb);
}

static __forceinline ULONGLONG AerovblkBe64ToCpu(_In_reads_bytes_(8) const UCHAR* p) {
  return ((ULONGLONG)p[0] << 56) | ((ULONGLONG)p[1] << 48) | ((ULONGLONG)p[2] << 40) | ((ULONGLONG)p[3] << 32) |
         ((ULONGLONG)p[4] << 24) | ((ULONGLONG)p[5] << 16) | ((ULONGLONG)p[6] << 8) | ((ULONGLONG)p[7]);
}

static __forceinline ULONG AerovblkBe32ToCpu(_In_reads_bytes_(4) const UCHAR* p) {
  return ((ULONG)p[0] << 24) | ((ULONG)p[1] << 16) | ((ULONG)p[2] << 8) | ((ULONG)p[3]);
}

static __forceinline USHORT AerovblkBe16ToCpu(_In_reads_bytes_(2) const UCHAR* p) { return (USHORT)(((USHORT)p[0] << 8) | (USHORT)p[1]); }

static VOID AerovblkWriteBe32(_Out_writes_bytes_(4) UCHAR* p, _In_ ULONG v) {
  p[0] = (UCHAR)(v >> 24);
  p[1] = (UCHAR)(v >> 16);
  p[2] = (UCHAR)(v >> 8);
  p[3] = (UCHAR)v;
}

static VOID AerovblkWriteBe64(_Out_writes_bytes_(8) UCHAR* p, _In_ ULONGLONG v) {
  p[0] = (UCHAR)(v >> 56);
  p[1] = (UCHAR)(v >> 48);
  p[2] = (UCHAR)(v >> 40);
  p[3] = (UCHAR)(v >> 32);
  p[4] = (UCHAR)(v >> 24);
  p[5] = (UCHAR)(v >> 16);
  p[6] = (UCHAR)(v >> 8);
  p[7] = (UCHAR)v;
}

static __forceinline ULONG AerovblkSectorsPerLogicalBlock(_In_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  if (devExt->LogicalSectorSize < AEROVBLK_LOGICAL_SECTOR_SIZE) {
    return 1;
  }
  if ((devExt->LogicalSectorSize % AEROVBLK_LOGICAL_SECTOR_SIZE) != 0) {
    return 1;
  }
  return devExt->LogicalSectorSize / AEROVBLK_LOGICAL_SECTOR_SIZE;
}

static __forceinline ULONGLONG AerovblkTotalLogicalBlocks(_In_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  ULONGLONG capBytes;

  if (devExt->LogicalSectorSize == 0) {
    return 0;
  }

  capBytes = devExt->CapacitySectors * (ULONGLONG)AEROVBLK_LOGICAL_SECTOR_SIZE;
  return capBytes / (ULONGLONG)devExt->LogicalSectorSize;
}

static VOID AerovblkResetRequestContextsLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
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

static VOID AerovblkAbortOutstandingRequestsLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
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

static BOOLEAN AerovblkAllocateRequestContexts(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  ULONG i;
  ULONG ctxCount;
  PHYSICAL_ADDRESS low;
  PHYSICAL_ADDRESS high;
  PHYSICAL_ADDRESS boundary;
  PVOID pageVa;
  ULONG pageLen;
  STOR_PHYSICAL_ADDRESS pagePa;
  PAEROVBLK_REQUEST_CONTEXT ctx;

  ctxCount = (ULONG)devExt->Vq.QueueSize;
  devExt->RequestContextCount = ctxCount;

  devExt->RequestContexts = (PAEROVBLK_REQUEST_CONTEXT)StorPortAllocatePool(devExt, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount, 'bVrA');
  if (devExt->RequestContexts == NULL) {
    return FALSE;
  }

  RtlZeroMemory(devExt->RequestContexts, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount);

  InitializeListHead(&devExt->FreeRequestList);
  devExt->FreeRequestCount = 0;

  low.QuadPart = 0;
  high.QuadPart = 0xFFFFFFFFull;
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
    ctx->SharedPagePa.QuadPart = pagePa.QuadPart;
    ctx->ReqHdr = (volatile VIRTIO_BLK_REQ_HDR*)((PUCHAR)pageVa + AEROVBLK_CTX_HDR_OFFSET);
    ctx->StatusByte = (volatile UCHAR*)((PUCHAR)pageVa + AEROVBLK_CTX_STATUS_OFFSET);
    ctx->TableDesc = (volatile VRING_DESC*)((PUCHAR)pageVa + AEROVBLK_CTX_TABLE_OFFSET);
    ctx->TableDescPa.QuadPart = ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_TABLE_OFFSET;
    ctx->Sg = (volatile VIRTIO_SG_ENTRY*)((PUCHAR)pageVa + AEROVBLK_CTX_TABLE_OFFSET);

    ctx->Srb = NULL;
    ctx->IsWrite = FALSE;

    InsertTailList(&devExt->FreeRequestList, &ctx->Link);
    devExt->FreeRequestCount++;
  }

  return TRUE;
}

static BOOLEAN AerovblkDeviceBringUp(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ BOOLEAN allocateResources) {
  ULONG hostFeatures;
  ULONG wanted;
  UCHAR status;
  NTSTATUS st;
  VIRTIO_BLK_CONFIG cfg;
  STOR_LOCK_HANDLE lock;

  VirtioPciReset(&devExt->Vdev);

  if (!allocateResources) {
    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
    AerovblkAbortOutstandingRequestsLocked(devExt);
    VirtioQueueResetState(&devExt->Vq);
    StorPortReleaseSpinLock(devExt, &lock);
  }

  VirtioPciSetStatus(&devExt->Vdev, VIRTIO_STATUS_ACKNOWLEDGE);
  VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_DRIVER);

  hostFeatures = VirtioPciReadHostFeatures(&devExt->Vdev);
  wanted = VIRTIO_RING_F_INDIRECT_DESC | VIRTIO_BLK_F_FLUSH | VIRTIO_BLK_F_BLK_SIZE | VIRTIO_BLK_F_SEG_MAX | VIRTIO_BLK_F_SIZE_MAX;

  devExt->NegotiatedFeatures = hostFeatures & wanted;
  devExt->SupportsIndirect = (devExt->NegotiatedFeatures & VIRTIO_RING_F_INDIRECT_DESC) ? TRUE : FALSE;
  devExt->SupportsFlush = (devExt->NegotiatedFeatures & VIRTIO_BLK_F_FLUSH) ? TRUE : FALSE;

  VirtioPciWriteGuestFeatures(&devExt->Vdev, devExt->NegotiatedFeatures);

  VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_FEATURES_OK);
  status = VirtioPciGetStatus(&devExt->Vdev);
  if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
    VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  if (allocateResources) {
    st = VirtioQueueCreate(&devExt->Vdev, &devExt->Vq, 0);
    if (!NT_SUCCESS(st)) {
      VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_FAILED);
      return FALSE;
    }

    if (!AerovblkAllocateRequestContexts(devExt)) {
      VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_FAILED);
      return FALSE;
    }
  } else {
    VirtioPciSelectQueue(&devExt->Vdev, devExt->Vq.QueueIndex);
    VirtioPciWriteQueuePfn(&devExt->Vdev, (ULONG)(devExt->Vq.RingPa.QuadPart >> PAGE_SHIFT));
  }

  RtlZeroMemory(&cfg, sizeof(cfg));
  st = VirtioPciReadDeviceConfig(&devExt->Vdev, 0, &cfg, sizeof(cfg));
  if (!NT_SUCCESS(st)) {
    cfg.Capacity = 0;
    cfg.BlkSize = 0;
    cfg.SegMax = 0;
    cfg.SizeMax = 0;
  }

  devExt->CapacitySectors = cfg.Capacity;
  devExt->LogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
  devExt->SegMax = 0;
  devExt->SizeMax = 0;

  if ((devExt->NegotiatedFeatures & VIRTIO_BLK_F_BLK_SIZE) && cfg.BlkSize >= AEROVBLK_LOGICAL_SECTOR_SIZE &&
      (cfg.BlkSize % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0) {
    devExt->LogicalSectorSize = cfg.BlkSize;
  }

  if (devExt->NegotiatedFeatures & VIRTIO_BLK_F_SEG_MAX) {
    devExt->SegMax = cfg.SegMax;
    if (devExt->SegMax > (ULONG)AEROVBLK_MAX_SG_ELEMENTS) {
      devExt->SegMax = (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
    }
  }

  if (devExt->NegotiatedFeatures & VIRTIO_BLK_F_SIZE_MAX) {
    devExt->SizeMax = cfg.SizeMax;
  }

  VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_DRIVER_OK);
  StorPortNotification(NextRequest, devExt, NULL);
  return TRUE;
}

static BOOLEAN AerovblkQueueRequest(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ ULONG reqType,
                                   _In_ ULONGLONG startSector, _In_opt_ PSTOR_SCATTER_GATHER_LIST sg, _In_ BOOLEAN isWrite) {
  STOR_LOCK_HANDLE lock;
  LIST_ENTRY* entry;
  PAEROVBLK_REQUEST_CONTEXT ctx;
  ULONG sgCount;
  USHORT totalDesc;
  USHORT statusIdx;
  NTSTATUS st;
  USHORT headId;
  ULONG i;

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  if (devExt->Removed) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkSetSense(devExt, srb, SCSI_SENSE_NOT_READY, 0x04, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    return TRUE;
  }

  sgCount = (sg == NULL) ? 0 : sg->NumberOfElements;

  if (sgCount > (ULONG)AEROVBLK_MAX_SG_ELEMENTS) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
    return TRUE;
  }

  if (devExt->SegMax != 0 && sgCount > devExt->SegMax) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
    return TRUE;
  }

  if (!devExt->SupportsIndirect && (sgCount + 2) > (ULONG)devExt->Vq.QueueSize) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
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

  if (devExt->SupportsIndirect) {
    totalDesc = (USHORT)(sgCount + 2);
    if (totalDesc > AEROVBLK_MAX_TABLE_DESCS) {
      ctx->Srb = NULL;
      InsertTailList(&devExt->FreeRequestList, &ctx->Link);
      devExt->FreeRequestCount++;
      StorPortReleaseSpinLock(devExt, &lock);

      AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x55, 0x00);
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
      return TRUE;
    }

    statusIdx = (USHORT)(1 + sgCount);

    ctx->TableDesc[0].Addr = (ULONGLONG)(ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_HDR_OFFSET);
    ctx->TableDesc[0].Len = sizeof(VIRTIO_BLK_REQ_HDR);
    ctx->TableDesc[0].Flags = VRING_DESC_F_NEXT;
    ctx->TableDesc[0].Next = statusIdx ? 1 : 0;

    for (i = 0; i < sgCount; ++i) {
      USHORT idx;
      idx = (USHORT)(1 + i);
      ctx->TableDesc[idx].Addr = (ULONGLONG)sg->Elements[i].PhysicalAddress.QuadPart;
      ctx->TableDesc[idx].Len = sg->Elements[i].Length;
      ctx->TableDesc[idx].Flags = (USHORT)(isWrite ? 0 : VRING_DESC_F_WRITE);
      ctx->TableDesc[idx].Flags |= VRING_DESC_F_NEXT;
      ctx->TableDesc[idx].Next = (USHORT)(idx + 1);
    }

    ctx->TableDesc[statusIdx].Addr = (ULONGLONG)(ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_STATUS_OFFSET);
    ctx->TableDesc[statusIdx].Len = 1;
    ctx->TableDesc[statusIdx].Flags = VRING_DESC_F_WRITE;
    ctx->TableDesc[statusIdx].Next = 0;

    if (sgCount == 0) {
      ctx->TableDesc[0].Next = statusIdx;
    }

    st = VirtioQueueAddIndirectTable(&devExt->Vq, ctx->TableDescPa, totalDesc, ctx, &headId);
  } else {
    totalDesc = (USHORT)(sgCount + 2);

    ctx->Sg[0].Address.QuadPart = ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_HDR_OFFSET;
    ctx->Sg[0].Length = sizeof(VIRTIO_BLK_REQ_HDR);
    ctx->Sg[0].Write = FALSE;

    for (i = 0; i < sgCount; ++i) {
      ctx->Sg[1 + i].Address = sg->Elements[i].PhysicalAddress;
      ctx->Sg[1 + i].Length = sg->Elements[i].Length;
      ctx->Sg[1 + i].Write = isWrite ? FALSE : TRUE;
    }

    ctx->Sg[1 + sgCount].Address.QuadPart = ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_STATUS_OFFSET;
    ctx->Sg[1 + sgCount].Length = 1;
    ctx->Sg[1 + sgCount].Write = TRUE;

    st = VirtioQueueAddBuffer(&devExt->Vq, (const VIRTIO_SG_ENTRY*)ctx->Sg, totalDesc, ctx, &headId);
  }

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

  VirtioQueueNotify(&devExt->Vdev, &devExt->Vq);

  StorPortReleaseSpinLock(devExt, &lock);
  StorPortNotification(NextRequest, devExt, NULL);
  return TRUE;
}

static VOID AerovblkHandleInquiry(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb) {
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

static VOID AerovblkHandleReadCapacity10(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb) {
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

static VOID AerovblkHandleReadCapacity16(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb) {
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

static VOID AerovblkHandleModeSense(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ BOOLEAN mode10) {
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

static VOID AerovblkHandleRequestSense(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb) {
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

static VOID AerovblkHandleIoControl(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb) {
  PSRB_IO_CONTROL ctrl;
  PAEROVBLK_QUERY_INFO info;

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
  info->NegotiatedFeatures = devExt->NegotiatedFeatures;
  info->QueueSize = devExt->Vq.QueueSize;
  info->FreeCount = devExt->Vq.NumFree;
  info->AvailIdx = devExt->Vq.Avail->Idx;
  info->UsedIdx = devExt->Vq.Used->Idx;

  ctrl->ReturnCode = 0;
  ctrl->Length = sizeof(AEROVBLK_QUERY_INFO);
  srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + sizeof(AEROVBLK_QUERY_INFO);
  AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
}

static VOID AerovblkHandleUnsupported(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb) {
  AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x20, 0x00);
  AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
}

ULONG DriverEntry(_In_ PDRIVER_OBJECT driverObject, _In_ PUNICODE_STRING registryPath) {
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

ULONG AerovblkHwFindAdapter(_In_ PVOID deviceExtension, _In_ PVOID hwContext, _In_ PVOID busInformation, _In_ PCHAR argumentString,
                           _Inout_ PPORT_CONFIGURATION_INFORMATION configInfo, _Out_ PBOOLEAN again) {
  PAEROVBLK_DEVICE_EXTENSION devExt;
  PACCESS_RANGE range;
  PVOID base;
  USHORT hwQueueSize;
  ULONG hostFeatures;
  ULONG maxPhysBreaks;
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
  if (range->RangeInMemory) {
    return SP_RETURN_NOT_FOUND;
  }

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  RtlZeroMemory(devExt, sizeof(*devExt));

  base = StorPortGetDeviceBase(devExt, configInfo->AdapterInterfaceType, configInfo->SystemIoBusNumber, range->RangeStart, range->RangeLength, TRUE);
  if (base == NULL) {
    return SP_RETURN_NOT_FOUND;
  }

  VirtioPciInitialize(&devExt->Vdev, (PUCHAR)base, range->RangeLength, FALSE);

  VirtioPciSelectQueue(&devExt->Vdev, 0);
  hwQueueSize = VirtioPciReadQueueSize(&devExt->Vdev);
  if (hwQueueSize == 0) {
    return SP_RETURN_NOT_FOUND;
  }

  hostFeatures = VirtioPciReadHostFeatures(&devExt->Vdev);

  maxPhysBreaks = 17;
  if (hostFeatures & VIRTIO_RING_F_INDIRECT_DESC) {
    maxPhysBreaks = (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
  } else if (hwQueueSize > 2) {
    maxPhysBreaks = (ULONG)(hwQueueSize - 2);
  }

  RtlZeroMemory(&blkCfg, sizeof(blkCfg));
  st = VirtioPciReadDeviceConfig(&devExt->Vdev, 0, &blkCfg, sizeof(blkCfg));
  if (NT_SUCCESS(st) && (hostFeatures & VIRTIO_BLK_F_SEG_MAX) && blkCfg.SegMax != 0 && blkCfg.SegMax < maxPhysBreaks) {
    maxPhysBreaks = blkCfg.SegMax;
  }

  if (maxPhysBreaks > (ULONG)AEROVBLK_MAX_SG_ELEMENTS) {
    maxPhysBreaks = (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
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
  configInfo->AlignmentMask = AEROVBLK_LOGICAL_SECTOR_SIZE - 1;
  configInfo->MaximumTransferLength = 1024 * 1024;
  configInfo->NumberOfPhysicalBreaks = maxPhysBreaks;

  return SP_RETURN_FOUND;
}

BOOLEAN AerovblkHwInitialize(_In_ PVOID deviceExtension) {
  PAEROVBLK_DEVICE_EXTENSION devExt;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  return AerovblkDeviceBringUp(devExt, TRUE);
}

BOOLEAN AerovblkHwResetBus(_In_ PVOID deviceExtension, _In_ ULONG pathId) {
  PAEROVBLK_DEVICE_EXTENSION devExt;

  UNREFERENCED_PARAMETER(pathId);

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  return AerovblkDeviceBringUp(devExt, FALSE);
}

SCSI_ADAPTER_CONTROL_STATUS AerovblkHwAdapterControl(_In_ PVOID deviceExtension, _In_ SCSI_ADAPTER_CONTROL_TYPE controlType,
                                                     _In_ PVOID parameters) {
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

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
    AerovblkAbortOutstandingRequestsLocked(devExt);
    VirtioQueueResetState(&devExt->Vq);
    StorPortReleaseSpinLock(devExt, &lock);

    VirtioPciReset(&devExt->Vdev);
    return ScsiAdapterControlSuccess;
  }

  case ScsiRestartAdapter:
    devExt->Removed = FALSE;
    return AerovblkDeviceBringUp(devExt, FALSE) ? ScsiAdapterControlSuccess : ScsiAdapterControlUnsuccessful;

  default:
    return ScsiAdapterControlUnsuccessful;
  }
}

BOOLEAN AerovblkHwInterrupt(_In_ PVOID deviceExtension) {
  PAEROVBLK_DEVICE_EXTENSION devExt;
  UCHAR isr;
  STOR_LOCK_HANDLE lock;
  USHORT headId;
  ULONG usedLen;
  PVOID ctxPtr;
  PAEROVBLK_REQUEST_CONTEXT ctx;
  PSCSI_REQUEST_BLOCK srb;
  UCHAR statusByte;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

  isr = VirtioPciReadIsr(&devExt->Vdev);
  if (isr == 0) {
    return FALSE;
  }

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  while (VirtioQueuePopUsed(&devExt->Vq, &headId, &usedLen, &ctxPtr)) {
    UNREFERENCED_PARAMETER(headId);
    UNREFERENCED_PARAMETER(usedLen);

    ctx = (PAEROVBLK_REQUEST_CONTEXT)ctxPtr;
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

BOOLEAN AerovblkHwStartIo(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb) {
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
