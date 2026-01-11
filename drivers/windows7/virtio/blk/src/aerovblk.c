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

  ctxCount = (devExt->Vq != NULL) ? (ULONG)devExt->Vq->qsz : 0;
  if (ctxCount == 0) {
    return FALSE;
  }
  devExt->RequestContextCount = ctxCount;

  devExt->RequestContexts = (PAEROVBLK_REQUEST_CONTEXT)StorPortAllocatePool(devExt, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount, 'bVrA');
  if (devExt->RequestContexts == NULL) {
    return FALSE;
  }

  RtlZeroMemory(devExt->RequestContexts, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount);

  InitializeListHead(&devExt->FreeRequestList);
  devExt->FreeRequestCount = 0;

  low.QuadPart = 0;
  high.QuadPart = (LONGLONG)-1;
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

    ctx->Srb = NULL;
    ctx->IsWrite = FALSE;

    InsertTailList(&devExt->FreeRequestList, &ctx->Link);
    devExt->FreeRequestCount++;
  }

  return TRUE;
}

/* -------------------------------------------------------------------------- */
/* Virtio PCI modern MMIO helpers (BAR0 fixed layout, contract v1)             */
/* -------------------------------------------------------------------------- */

#define AEROVBLK_MMIO_READ8(p) READ_REGISTER_UCHAR((volatile UCHAR*)(p))
#define AEROVBLK_MMIO_READ16(p) READ_REGISTER_USHORT((volatile USHORT*)(p))
#define AEROVBLK_MMIO_READ32(p) READ_REGISTER_ULONG((volatile ULONG*)(p))

#define AEROVBLK_MMIO_WRITE8(p, v) WRITE_REGISTER_UCHAR((volatile UCHAR*)(p), (UCHAR)(v))
#define AEROVBLK_MMIO_WRITE16(p, v) WRITE_REGISTER_USHORT((volatile USHORT*)(p), (USHORT)(v))
#define AEROVBLK_MMIO_WRITE32(p, v) WRITE_REGISTER_ULONG((volatile ULONG*)(p), (ULONG)(v))

static __forceinline ULONGLONG AerovblkMmioRead64(_In_ volatile UCHAR* p) {
  ULONG lo;
  ULONG hi;

  lo = AEROVBLK_MMIO_READ32(p);
  hi = AEROVBLK_MMIO_READ32(p + 4);
  return (ULONGLONG)lo | ((ULONGLONG)hi << 32);
}

static __forceinline VOID AerovblkMmioWrite64(_In_ volatile UCHAR* p, _In_ ULONGLONG v) {
  AEROVBLK_MMIO_WRITE32(p, (ULONG)v);
  AEROVBLK_MMIO_WRITE32(p + 4, (ULONG)(v >> 32));
}

/* Offsets within the common configuration block (BAR0 + 0x0000). */
#define VIRTIO_PCI_COMMON_DFSELECT 0x00u
#define VIRTIO_PCI_COMMON_DFEATURE 0x04u
#define VIRTIO_PCI_COMMON_GFSELECT 0x08u
#define VIRTIO_PCI_COMMON_GFEATURE 0x0Cu
#define VIRTIO_PCI_COMMON_MSIX_CONFIG 0x10u
#define VIRTIO_PCI_COMMON_NUM_QUEUES 0x12u
#define VIRTIO_PCI_COMMON_DEVICE_STATUS 0x14u
#define VIRTIO_PCI_COMMON_QUEUE_SELECT 0x16u
#define VIRTIO_PCI_COMMON_QUEUE_SIZE 0x18u
#define VIRTIO_PCI_COMMON_QUEUE_MSIX_VECTOR 0x1Au
#define VIRTIO_PCI_COMMON_QUEUE_ENABLE 0x1Cu
#define VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF 0x1Eu
#define VIRTIO_PCI_COMMON_QUEUE_DESC 0x20u
#define VIRTIO_PCI_COMMON_QUEUE_AVAIL 0x28u
#define VIRTIO_PCI_COMMON_QUEUE_USED 0x30u

static __forceinline UCHAR AerovblkVirtioGetStatus(_In_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  return AEROVBLK_MMIO_READ8(devExt->CommonCfg + VIRTIO_PCI_COMMON_DEVICE_STATUS);
}

static __forceinline VOID AerovblkVirtioSetStatus(_In_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ UCHAR status) {
  AEROVBLK_MMIO_WRITE8(devExt->CommonCfg + VIRTIO_PCI_COMMON_DEVICE_STATUS, status);
}

static __forceinline VOID AerovblkVirtioAddStatus(_In_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ UCHAR bits) {
  UCHAR status;

  status = AerovblkVirtioGetStatus(devExt);
  AerovblkVirtioSetStatus(devExt, (UCHAR)(status | bits));
}

static BOOLEAN AerovblkVirtioResetDevice(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  ULONG i;

  /* Writing 0 resets the device. */
  AerovblkVirtioSetStatus(devExt, 0);

  /* Poll until the device acknowledges the reset (bounded). */
  for (i = 0; i < 1000; ++i) {
    if (AerovblkVirtioGetStatus(devExt) == 0) {
      return TRUE;
    }
    StorPortStallExecution(devExt, 1000); /* 1ms */
  }

  return FALSE;
}

static ULONGLONG AerovblkVirtioReadDeviceFeatures(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  ULONG lo;
  ULONG hi;

  AEROVBLK_MMIO_WRITE32(devExt->CommonCfg + VIRTIO_PCI_COMMON_DFSELECT, 0);
  lo = AEROVBLK_MMIO_READ32(devExt->CommonCfg + VIRTIO_PCI_COMMON_DFEATURE);

  AEROVBLK_MMIO_WRITE32(devExt->CommonCfg + VIRTIO_PCI_COMMON_DFSELECT, 1);
  hi = AEROVBLK_MMIO_READ32(devExt->CommonCfg + VIRTIO_PCI_COMMON_DFEATURE);

  return (ULONGLONG)lo | ((ULONGLONG)hi << 32);
}

static VOID AerovblkVirtioWriteDriverFeatures(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ ULONGLONG features) {
  AEROVBLK_MMIO_WRITE32(devExt->CommonCfg + VIRTIO_PCI_COMMON_GFSELECT, 0);
  AEROVBLK_MMIO_WRITE32(devExt->CommonCfg + VIRTIO_PCI_COMMON_GFEATURE, (ULONG)features);

  AEROVBLK_MMIO_WRITE32(devExt->CommonCfg + VIRTIO_PCI_COMMON_GFSELECT, 1);
  AEROVBLK_MMIO_WRITE32(devExt->CommonCfg + VIRTIO_PCI_COMMON_GFEATURE, (ULONG)(features >> 32));
}

static VOID AerovblkVirtioReadBlkConfig(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Out_ PVIRTIO_BLK_CONFIG cfg) {
  RtlZeroMemory(cfg, sizeof(*cfg));

  cfg->Capacity = AerovblkMmioRead64(devExt->DeviceCfg + 0x00u);
  cfg->SegMax = AEROVBLK_MMIO_READ32(devExt->DeviceCfg + 0x0Cu);
  cfg->BlkSize = AEROVBLK_MMIO_READ32(devExt->DeviceCfg + 0x14u);
}

static VOID AerovblkVirtioNotifyQueue0(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  volatile UCHAR* notifyAddr;

  notifyAddr = devExt->NotifyBase + ((ULONG)devExt->QueueNotifyOff * devExt->NotifyOffMultiplier);

  /* Contract v1: accept 16-bit or 32-bit writes; use 16-bit. */
  AEROVBLK_MMIO_WRITE16(notifyAddr, (USHORT)AEROVBLK_QUEUE_INDEX);
}

static BOOLEAN AerovblkVirtioSetupQueue0(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  USHORT qsz;

  AEROVBLK_MMIO_WRITE16(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_SELECT, (USHORT)AEROVBLK_QUEUE_INDEX);
  qsz = AEROVBLK_MMIO_READ16(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_SIZE);
  if (qsz != (USHORT)AEROVBLK_QUEUE_SIZE) {
    return FALSE;
  }

  devExt->QueueNotifyOff = AEROVBLK_MMIO_READ16(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF);

  /* Disable MSI-X vectors (INTx required by contract v1). */
  AEROVBLK_MMIO_WRITE16(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_MSIX_VECTOR, 0xFFFFu);

  /* Program split ring addresses and enable the queue. */
  AerovblkMmioWrite64(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_DESC, devExt->Vq->desc_pa);
  AerovblkMmioWrite64(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_AVAIL, devExt->Vq->avail_pa);
  AerovblkMmioWrite64(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_USED, devExt->Vq->used_pa);
  AEROVBLK_MMIO_WRITE16(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_ENABLE, 1);

  return TRUE;
}

static BOOLEAN AerovblkAllocateVirtqueue(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  PHYSICAL_ADDRESS low;
  PHYSICAL_ADDRESS high;
  PHYSICAL_ADDRESS boundary;
  size_t ringBytes;
  ULONG ringLen;
  STOR_PHYSICAL_ADDRESS ringPa;
  PVOID ringVa;
  size_t indirectBytes;
  ULONG indirectLen;
  STOR_PHYSICAL_ADDRESS indirectPa;
  PVOID indirectVa;
  size_t vqBytes;
  NTSTATUS st;

  if (devExt->Vq != NULL) {
    return TRUE;
  }

  low.QuadPart = 0;
  high.QuadPart = (LONGLONG)-1;
  boundary.QuadPart = 0;

  ringBytes = VirtqSplitRingMemSize((UINT16)AEROVBLK_QUEUE_SIZE, 4, FALSE);
  if (ringBytes == 0 || ringBytes > 0xFFFFFFFFu) {
    return FALSE;
  }

  ringVa = StorPortAllocateContiguousMemorySpecifyCache(devExt, (ULONG)ringBytes, low, high, boundary, MmNonCached);
  if (ringVa == NULL) {
    return FALSE;
  }

  ringLen = (ULONG)ringBytes;
  ringPa = StorPortGetPhysicalAddress(devExt, NULL, ringVa, &ringLen);
  if (ringLen < ringBytes) {
    return FALSE;
  }

  RtlZeroMemory(ringVa, ringBytes);

  devExt->RingVa = ringVa;
  devExt->RingPa.QuadPart = ringPa.QuadPart;
  devExt->RingBytes = (ULONG)ringBytes;

  devExt->IndirectTableCount = (USHORT)AEROVBLK_QUEUE_SIZE;
  devExt->IndirectMaxDesc = (USHORT)(devExt->SegMax + 2u);
  if (devExt->IndirectMaxDesc < 2) {
    devExt->IndirectMaxDesc = 2;
  }

  indirectBytes = (size_t)devExt->IndirectTableCount * (size_t)devExt->IndirectMaxDesc * sizeof(VIRTQ_DESC);
  if (indirectBytes == 0 || indirectBytes > 0xFFFFFFFFu) {
    return FALSE;
  }

  indirectVa = StorPortAllocateContiguousMemorySpecifyCache(devExt, (ULONG)indirectBytes, low, high, boundary, MmNonCached);
  if (indirectVa == NULL) {
    return FALSE;
  }

  indirectLen = (ULONG)indirectBytes;
  indirectPa = StorPortGetPhysicalAddress(devExt, NULL, indirectVa, &indirectLen);
  if (indirectLen < indirectBytes) {
    return FALSE;
  }

  RtlZeroMemory(indirectVa, indirectBytes);

  devExt->IndirectVa = indirectVa;
  devExt->IndirectPa.QuadPart = indirectPa.QuadPart;
  devExt->IndirectBytes = (ULONG)indirectBytes;

  vqBytes = VirtqSplitStateSize((UINT16)AEROVBLK_QUEUE_SIZE);
  if (vqBytes == 0 || vqBytes > 0xFFFFFFFFu) {
    return FALSE;
  }

  devExt->Vq = (VIRTQ_SPLIT*)StorPortAllocatePool(devExt, (ULONG)vqBytes, 'qVrA');
  if (devExt->Vq == NULL) {
    return FALSE;
  }

  st = VirtqSplitInit(devExt->Vq,
                      (UINT16)AEROVBLK_QUEUE_SIZE,
                      FALSE,
                      TRUE,
                      ringVa,
                      (UINT64)ringPa.QuadPart,
                      4,
                      indirectVa,
                      (UINT64)indirectPa.QuadPart,
                      devExt->IndirectTableCount,
                      devExt->IndirectMaxDesc);
  if (!NT_SUCCESS(st)) {
    return FALSE;
  }

  /* Prefer indirect for all requests (contract v1 requires indirect support). */
  devExt->Vq->indirect_threshold = 0;

  return TRUE;
}

static BOOLEAN AerovblkDeviceBringUp(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ BOOLEAN allocateResources) {
  STOR_LOCK_HANDLE lock;
  ULONGLONG hostFeatures;
  ULONGLONG required;
  ULONGLONG negotiated;
  VIRTIO_BLK_CONFIG cfg;
  USHORT numQueues;
  UCHAR status;

  if (devExt->CommonCfg == NULL || devExt->DeviceCfg == NULL) {
    return FALSE;
  }

  devExt->NotifyOffMultiplier = AEROVBLK_VIRTIO_PCI_NOTIFY_OFF_MULTIPLIER;

  if (!AerovblkVirtioResetDevice(devExt)) {
    return FALSE;
  }

  if (!allocateResources) {
    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
    AerovblkAbortOutstandingRequestsLocked(devExt);
    if (devExt->Vq != NULL) {
      VirtqSplitReset(devExt->Vq);
    }
    StorPortReleaseSpinLock(devExt, &lock);
  }

  /* ACKNOWLEDGE | DRIVER */
  AerovblkVirtioSetStatus(devExt, (UCHAR)(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER));

  /* Disable MSI-X config vector (INTx required). */
  AEROVBLK_MMIO_WRITE16(devExt->CommonCfg + VIRTIO_PCI_COMMON_MSIX_CONFIG, 0xFFFFu);

  numQueues = AEROVBLK_MMIO_READ16(devExt->CommonCfg + VIRTIO_PCI_COMMON_NUM_QUEUES);
  if (numQueues == 0) {
    AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  hostFeatures = AerovblkVirtioReadDeviceFeatures(devExt);
  required = AEROVBLK_FEATURE_VERSION_1 | AEROVBLK_FEATURE_RING_INDIRECT_DESC | AEROVBLK_FEATURE_BLK_SEG_MAX | AEROVBLK_FEATURE_BLK_BLK_SIZE |
             AEROVBLK_FEATURE_BLK_FLUSH;

  if ((hostFeatures & required) != required) {
    AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  negotiated = required;
  AerovblkVirtioWriteDriverFeatures(devExt, negotiated);

  AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FEATURES_OK);
  status = AerovblkVirtioGetStatus(devExt);
  if ((status & VIRTIO_STATUS_FEATURES_OK) == 0) {
    AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  devExt->NegotiatedFeatures = negotiated;
  devExt->SupportsIndirect = TRUE;
  devExt->SupportsFlush = TRUE;

  AerovblkVirtioReadBlkConfig(devExt, &cfg);

  devExt->CapacitySectors = cfg.Capacity;
  devExt->LogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
  if (cfg.BlkSize >= AEROVBLK_LOGICAL_SECTOR_SIZE && (cfg.BlkSize % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0) {
    devExt->LogicalSectorSize = cfg.BlkSize;
  }

  devExt->SegMax = (cfg.SegMax != 0) ? cfg.SegMax : (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
  if (devExt->SegMax > (ULONG)AEROVBLK_MAX_SG_ELEMENTS) {
    devExt->SegMax = (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
  }

  if (allocateResources) {
    if (!AerovblkAllocateVirtqueue(devExt)) {
      AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FAILED);
      return FALSE;
    }

    if (!AerovblkAllocateRequestContexts(devExt)) {
      AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FAILED);
      return FALSE;
    }
  } else {
    if (devExt->Vq == NULL || devExt->RequestContexts == NULL) {
      AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FAILED);
      return FALSE;
    }
  }

  if (!AerovblkVirtioSetupQueue0(devExt)) {
    AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  AerovblkVirtioAddStatus(devExt, VIRTIO_STATUS_DRIVER_OK);

  StorPortNotification(NextRequest, devExt, NULL);
  return TRUE;
}

static BOOLEAN AerovblkQueueRequest(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ ULONG reqType,
                                   _In_ ULONGLONG startSector, _In_opt_ PSTOR_SCATTER_GATHER_LIST sg, _In_ BOOLEAN isWrite) {
  STOR_LOCK_HANDLE lock;
  LIST_ENTRY* entry;
  PAEROVBLK_REQUEST_CONTEXT ctx;
  ULONG sgCount;
  UINT16 totalDesc;
  NTSTATUS st;
  UINT16 headId;
  ULONG i;
  VIRTQ_SG segs[AEROVBLK_MAX_SG_ELEMENTS + 2];
  BOOLEAN needKick;

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  if (devExt->Removed) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkSetSense(devExt, srb, SCSI_SENSE_NOT_READY, 0x04, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    return TRUE;
  }

  if (devExt->Vq == NULL) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
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
  ctx->ReqHdr->Ioprio = 0;
  ctx->ReqHdr->Sector = startSector;
  *ctx->StatusByte = 0xFF;

  totalDesc = (UINT16)(sgCount + 2u);

  segs[0].addr = (UINT64)(ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_HDR_OFFSET);
  segs[0].len = (UINT32)sizeof(VIRTIO_BLK_REQ_HDR);
  segs[0].write = FALSE;

  for (i = 0; i < sgCount; ++i) {
    segs[1 + i].addr = (UINT64)sg->Elements[i].PhysicalAddress.QuadPart;
    segs[1 + i].len = (UINT32)sg->Elements[i].Length;
    segs[1 + i].write = isWrite ? FALSE : TRUE;
  }

  segs[1 + sgCount].addr = (UINT64)(ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_STATUS_OFFSET);
  segs[1 + sgCount].len = 1;
  segs[1 + sgCount].write = TRUE;

  st = VirtqSplitAddBuffer(devExt->Vq, segs, totalDesc, ctx, &headId);
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

  VirtqSplitPublish(devExt->Vq, headId);
  needKick = VirtqSplitKickPrepare(devExt->Vq);
  if (needKick) {
    AerovblkVirtioNotifyQueue0(devExt);
  }
  VirtqSplitKickCommit(devExt->Vq);

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
  if (devExt->Vq != NULL) {
    info->QueueSize = devExt->Vq->qsz;
    info->NumFree = devExt->Vq->num_free;
    info->AvailIdx = devExt->Vq->avail_idx;
    info->UsedIdx = VirtioReadU16((volatile UINT16*)&devExt->Vq->used->idx);
  } else {
    info->QueueSize = 0;
    info->NumFree = 0;
    info->AvailIdx = 0;
    info->UsedIdx = 0;
  }

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
  UCHAR pciCfg[0x40];
  ULONG bytesRead;
  USHORT hwQueueSize;
  ULONGLONG hostFeatures;
  ULONGLONG required;
  ULONG maxPhysBreaks;
  VIRTIO_BLK_CONFIG blkCfg;
  ULONG alignment;
  ULONG maxTransfer;

  UNREFERENCED_PARAMETER(hwContext);
  UNREFERENCED_PARAMETER(busInformation);
  UNREFERENCED_PARAMETER(argumentString);

  *again = FALSE;

  if (configInfo->NumberOfAccessRanges < 1) {
    return SP_RETURN_NOT_FOUND;
  }

  range = &configInfo->AccessRanges[0];
  if (!range->RangeInMemory) {
    return SP_RETURN_NOT_FOUND;
  }
  if (range->RangeLength < AEROVBLK_VIRTIO_PCI_BAR0_MIN_LEN) {
    return SP_RETURN_NOT_FOUND;
  }

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  RtlZeroMemory(devExt, sizeof(*devExt));

  /*
   * Contract v1 binds to PCI Revision ID 0x01.
   * Read directly from PCI config space via StorPort bus data access.
   */
  RtlZeroMemory(pciCfg, sizeof(pciCfg));
  bytesRead = StorPortGetBusData(devExt, PCIConfiguration, configInfo->SystemIoBusNumber, configInfo->SlotNumber, pciCfg, sizeof(pciCfg));
  if (bytesRead < 0x09 || pciCfg[0x08] != (UCHAR)AEROVBLK_VIRTIO_PCI_REVISION_ID) {
    return SP_RETURN_NOT_FOUND;
  }

  base = StorPortGetDeviceBase(devExt, configInfo->AdapterInterfaceType, configInfo->SystemIoBusNumber, range->RangeStart, range->RangeLength, FALSE);
  if (base == NULL) {
    return SP_RETURN_NOT_FOUND;
  }

  devExt->Bar0 = (PUCHAR)base;
  devExt->Bar0Length = range->RangeLength;

  devExt->CommonCfg = devExt->Bar0 + AEROVBLK_VIRTIO_PCI_COMMON_CFG_OFFSET;
  devExt->NotifyBase = devExt->Bar0 + AEROVBLK_VIRTIO_PCI_NOTIFY_CFG_OFFSET;
  devExt->IsrStatus = devExt->Bar0 + AEROVBLK_VIRTIO_PCI_ISR_CFG_OFFSET;
  devExt->DeviceCfg = devExt->Bar0 + AEROVBLK_VIRTIO_PCI_DEVICE_CFG_OFFSET;
  devExt->NotifyOffMultiplier = AEROVBLK_VIRTIO_PCI_NOTIFY_OFF_MULTIPLIER;
  devExt->QueueNotifyOff = 0;

  /* Validate queue 0 size (contract v1: 128). */
  AEROVBLK_MMIO_WRITE16(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_SELECT, (USHORT)AEROVBLK_QUEUE_INDEX);
  hwQueueSize = AEROVBLK_MMIO_READ16(devExt->CommonCfg + VIRTIO_PCI_COMMON_QUEUE_SIZE);
  if (hwQueueSize != (USHORT)AEROVBLK_QUEUE_SIZE) {
    return SP_RETURN_NOT_FOUND;
  }

  /* Validate required features are offered (contract v1). */
  hostFeatures = AerovblkVirtioReadDeviceFeatures(devExt);
  required = AEROVBLK_FEATURE_VERSION_1 | AEROVBLK_FEATURE_RING_INDIRECT_DESC | AEROVBLK_FEATURE_BLK_SEG_MAX | AEROVBLK_FEATURE_BLK_BLK_SIZE |
             AEROVBLK_FEATURE_BLK_FLUSH;
  if ((hostFeatures & required) != required) {
    return SP_RETURN_NOT_FOUND;
  }

  RtlZeroMemory(&blkCfg, sizeof(blkCfg));
  AerovblkVirtioReadBlkConfig(devExt, &blkCfg);

  maxPhysBreaks = (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
  if (blkCfg.SegMax != 0 && blkCfg.SegMax < maxPhysBreaks) {
    maxPhysBreaks = blkCfg.SegMax;
  }

  if (maxPhysBreaks > (ULONG)AEROVBLK_MAX_SG_ELEMENTS) {
    maxPhysBreaks = (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
  }

  devExt->LogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
  devExt->CapacitySectors = blkCfg.Capacity;
  if (blkCfg.BlkSize >= AEROVBLK_LOGICAL_SECTOR_SIZE && (blkCfg.BlkSize % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0) {
    devExt->LogicalSectorSize = blkCfg.BlkSize;
  }
  devExt->SegMax = maxPhysBreaks;
  devExt->Removed = FALSE;
  RtlZeroMemory(&devExt->LastSense, sizeof(devExt->LastSense));

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

  maxTransfer = 1024 * 1024;
  maxTransfer -= maxTransfer % AEROVBLK_LOGICAL_SECTOR_SIZE;
  if (maxTransfer == 0) {
    maxTransfer = AEROVBLK_LOGICAL_SECTOR_SIZE;
  }

  configInfo->AlignmentMask = alignment - 1;
  configInfo->MaximumTransferLength = maxTransfer;
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
    if (devExt->Vq != NULL) {
      VirtqSplitReset(devExt->Vq);
    }
    StorPortReleaseSpinLock(devExt, &lock);

    if (devExt->CommonCfg != NULL) {
      (void)AerovblkVirtioResetDevice(devExt);
    }
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
  PVOID ctxPtr;
  UINT32 usedLen;
  NTSTATUS st;
  PAEROVBLK_REQUEST_CONTEXT ctx;
  PSCSI_REQUEST_BLOCK srb;
  UCHAR statusByte;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

  if (devExt->IsrStatus == NULL) {
    return FALSE;
  }

  /*
   * Modern virtio-pci ISR byte (BAR0 + 0x2000). Read-to-ack.
   * Return FALSE if 0 for shared interrupt line safety.
   */
  isr = AEROVBLK_MMIO_READ8(devExt->IsrStatus);
  if (isr == 0) {
    return FALSE;
  }

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  if (devExt->Vq != NULL) {
    for (;;) {
      st = VirtqSplitGetUsed(devExt->Vq, &ctxPtr, &usedLen);
      if (st == STATUS_NOT_FOUND) {
        break;
      }
      if (!NT_SUCCESS(st)) {
        break;
      }

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

    if (bytes64 == 0 || bytes64 > 0xFFFFFFFFull || (bytes64 % AEROVBLK_LOGICAL_SECTOR_SIZE) != 0 ||
        srb->DataTransferLength != (ULONG)bytes64) {
      AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
      return TRUE;
    }

    if (virtioSector + sectorsLen > devExt->CapacitySectors) {
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

    if (bytes64 > 0xFFFFFFFFull || (bytes64 % AEROVBLK_LOGICAL_SECTOR_SIZE) != 0 || srb->DataTransferLength != (ULONG)bytes64) {
      AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
      return TRUE;
    }

    if (virtioSector + sectorsLen > devExt->CapacitySectors) {
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
