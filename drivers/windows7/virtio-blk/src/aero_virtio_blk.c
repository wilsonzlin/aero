#include "../include/aero_virtio_blk.h"

#include "virtio_pci_aero_layout_miniport.h"

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

static VOID AerovblkResetVirtqueueLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  if (devExt == NULL) {
    return;
  }

  virtqueue_split_reset(&devExt->Vq);
}

static VOID AerovblkFreeRequestContextsArray(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt,
                                            _Inout_updates_opt_(ctxCount) PAEROVBLK_REQUEST_CONTEXT ctxs,
                                            _In_ ULONG ctxCount) {
  ULONG i;

  if (devExt == NULL) {
    return;
  }

  if (ctxs == NULL) {
    return;
  }

  for (i = 0; i < ctxCount; ++i) {
    if (ctxs[i].SharedPageVa != NULL) {
      StorPortFreeContiguousMemorySpecifyCache(devExt, ctxs[i].SharedPageVa, PAGE_SIZE, MmNonCached);
      ctxs[i].SharedPageVa = NULL;
    }
  }

  StorPortFreePool(devExt, ctxs);
}

static VOID AerovblkFreeRequestContexts(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  PAEROVBLK_REQUEST_CONTEXT ctxs;
  ULONG ctxCount;

  if (devExt == NULL) {
    return;
  }

  /*
   * Always reset the free-list bookkeeping to avoid leaving the device
   * extension with list pointers that reference freed request contexts.
   */
  InitializeListHead(&devExt->FreeRequestList);
  devExt->FreeRequestCount = 0;

  ctxs = devExt->RequestContexts;
  ctxCount = devExt->RequestContextCount;
  devExt->RequestContexts = NULL;
  devExt->RequestContextCount = 0;

  AerovblkFreeRequestContextsArray(devExt, ctxs, ctxCount);
}

static VOID AerovblkFreeResources(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  STOR_LOCK_HANDLE lock;
  PAEROVBLK_REQUEST_CONTEXT requestContexts;
  ULONG requestContextCount;
  virtqueue_split_t vq;
  virtio_dma_buffer_t ringDma;

  if (devExt == NULL) {
    return;
  }

  /*
   * The caller must reset the device first so it cannot DMA into ring/request
   * memory while we free it.
   */

  /*
   * Detach shared resources under the interrupt spinlock so the interrupt
   * handler and StartIo path stop touching them before we free any backing
   * memory. We free outside the lock to avoid holding the spinlock across
   * potentially expensive memory manager operations.
   */
  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  AerovblkAbortOutstandingRequestsLocked(devExt);

  requestContexts = devExt->RequestContexts;
  requestContextCount = devExt->RequestContextCount;
  devExt->RequestContexts = NULL;
  devExt->RequestContextCount = 0;
  InitializeListHead(&devExt->FreeRequestList);
  devExt->FreeRequestCount = 0;

  vq = devExt->Vq;
  RtlZeroMemory(&devExt->Vq, sizeof(devExt->Vq));

  ringDma = devExt->RingDma;
  RtlZeroMemory(&devExt->RingDma, sizeof(devExt->RingDma));

  StorPortReleaseSpinLock(devExt, &lock);

  AerovblkFreeRequestContextsArray(devExt, requestContexts, requestContextCount);

  /*
   * Destroy the virtqueue (frees cookies + indirect tables) and free the split
   * ring DMA buffer allocated via virtqueue_split_alloc_ring.
   */
  virtqueue_split_destroy(&vq);
  virtqueue_split_free_ring(&devExt->VirtioOps, &devExt->VirtioOpsCtx, &ringDma);

  /*
   * Defensive: clear queue mapping bookkeeping used by VirtioPciGetQueueNotifyAddress.
   * It is safe to leave these intact, but clearing helps catch accidental reuse
   * after removal.
   */
  devExt->QueueNotifyAddrCache[0] = NULL;
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

  AerovblkFreeRequestContexts(devExt);

  ctxCount = (ULONG)devExt->Vq.queue_size;
  if (ctxCount == 0) {
    return FALSE;
  }
  devExt->RequestContextCount = ctxCount;

  devExt->RequestContexts = (PAEROVBLK_REQUEST_CONTEXT)StorPortAllocatePool(devExt, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount, 'bVrA');
  if (devExt->RequestContexts == NULL) {
    AerovblkFreeRequestContexts(devExt);
    return FALSE;
  }

  RtlZeroMemory(devExt->RequestContexts, sizeof(AEROVBLK_REQUEST_CONTEXT) * ctxCount);

  InitializeListHead(&devExt->FreeRequestList);
  devExt->FreeRequestCount = 0;

  low.QuadPart = 0;
  high.QuadPart = (LONGLONG)-1;
  boundary.QuadPart = 0;

  for (i = 0; i < ctxCount; ++i) {
    ctx = &devExt->RequestContexts[i];
    InitializeListHead(&ctx->Link);

    ctx->SharedPageVa = StorPortAllocateContiguousMemorySpecifyCache(devExt, PAGE_SIZE, low, high, boundary, MmNonCached);
    if (ctx->SharedPageVa == NULL) {
      AerovblkFreeRequestContexts(devExt);
      return FALSE;
    }

    pageVa = ctx->SharedPageVa;

    pageLen = PAGE_SIZE;
    pagePa = StorPortGetPhysicalAddress(devExt, NULL, pageVa, &pageLen);
    if (pageLen < PAGE_SIZE) {
      AerovblkFreeRequestContexts(devExt);
      return FALSE;
    }

    RtlZeroMemory(pageVa, PAGE_SIZE);

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

static VOID AerovblkFreeVirtqueue(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  if (devExt == NULL) {
    return;
  }

  virtqueue_split_destroy(&devExt->Vq);
  virtqueue_split_free_ring(&devExt->VirtioOps, &devExt->VirtioOpsCtx, &devExt->RingDma);
}

static VOID AerovblkFreeResources(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  if (devExt == NULL) {
    return;
  }

  AerovblkFreeRequestContexts(devExt);
  AerovblkFreeVirtqueue(devExt);
}

static NTSTATUS AerovblkVirtioReadBlkConfig(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Out_ PVIRTIO_BLK_CONFIG cfg) {
  if (cfg == NULL) {
    return STATUS_INVALID_PARAMETER;
  }

  RtlZeroMemory(cfg, sizeof(*cfg));
  return VirtioPciReadDeviceConfig(&devExt->Vdev, 0, cfg, sizeof(*cfg));
}

static VOID AerovblkVirtioNotifyQueue0(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  VirtioPciNotifyQueue(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX);
}

static BOOLEAN AerovblkAllocateVirtqueue(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  int vqRes;
  uint16_t indirectMaxDesc;

  if (devExt == NULL) {
    return FALSE;
  }

  if (devExt->Vq.queue_size != 0) {
    return TRUE;
  }

  if (!devExt->SupportsIndirect) {
    return FALSE;
  }

  vqRes = virtqueue_split_alloc_ring(&devExt->VirtioOps,
                                     &devExt->VirtioOpsCtx,
                                     (uint16_t)AEROVBLK_QUEUE_SIZE,
                                     16,
                                     VIRTIO_FALSE,
                                     &devExt->RingDma);
  if (vqRes != VIRTIO_OK) {
    return FALSE;
  }

  indirectMaxDesc = (uint16_t)(devExt->SegMax + 2u);
  if (indirectMaxDesc < 2u) {
    indirectMaxDesc = 2u;
  }

  vqRes = virtqueue_split_init(&devExt->Vq,
                               &devExt->VirtioOps,
                               &devExt->VirtioOpsCtx,
                               (uint16_t)AEROVBLK_QUEUE_INDEX,
                               (uint16_t)AEROVBLK_QUEUE_SIZE,
                               16,
                               &devExt->RingDma,
                               VIRTIO_FALSE,
                               VIRTIO_TRUE,
                               indirectMaxDesc);
  if (vqRes != VIRTIO_OK) {
    virtqueue_split_destroy(&devExt->Vq);
    virtqueue_split_free_ring(&devExt->VirtioOps, &devExt->VirtioOpsCtx, &devExt->RingDma);
    return FALSE;
  }

  return TRUE;
}

static BOOLEAN AerovblkDeviceBringUp(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ BOOLEAN allocateResources) {
  STOR_LOCK_HANDLE lock;
  UINT64 requiredFeatures;
  UINT64 negotiated;
  VIRTIO_BLK_CONFIG cfg;
  NTSTATUS st;
  USHORT queueSize;
  volatile UINT16* notifyAddr;
  volatile UINT16* expectedNotifyAddr;
  ULONGLONG notifyOffset;
  UINT64 descPa;
  UINT64 availPa;
  UINT64 usedPa;

  if (devExt->Vdev.CommonCfg == NULL || devExt->Vdev.DeviceCfg == NULL) {
    return FALSE;
  }

  devExt->Vdev.QueueNotifyAddrCache = devExt->QueueNotifyAddrCache;
  devExt->Vdev.QueueNotifyAddrCacheCount = RTL_NUMBER_OF(devExt->QueueNotifyAddrCache);

  if (!allocateResources) {
    /*
     * Reset the device first to stop DMA before touching ring memory or
     * completing outstanding SRBs. This matches the legacy driver's sequencing
     * (reset before abort/reset of software queue state) and avoids races where
     * the device could still be writing used-ring entries while we recycle
     * request contexts.
     */
    VirtioPciResetDevice(&devExt->Vdev);

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
    AerovblkAbortOutstandingRequestsLocked(devExt);
    if (devExt->Vq.queue_size != 0) {
      AerovblkResetVirtqueueLocked(devExt);
    }
    StorPortReleaseSpinLock(devExt, &lock);
  }

  requiredFeatures = AEROVBLK_FEATURE_RING_INDIRECT_DESC | AEROVBLK_FEATURE_BLK_SEG_MAX | AEROVBLK_FEATURE_BLK_BLK_SIZE | AEROVBLK_FEATURE_BLK_FLUSH;

  st = VirtioPciNegotiateFeatures(&devExt->Vdev, requiredFeatures, /*Wanted*/ 0, &negotiated);
  if (!NT_SUCCESS(st)) {
    return FALSE;
  }

  /* Disable MSI-X config vector (INTx required by contract v1). */
  WRITE_REGISTER_USHORT((volatile USHORT*)&devExt->Vdev.CommonCfg->msix_config, VIRTIO_PCI_MSI_NO_VECTOR);

  devExt->NegotiatedFeatures = negotiated;
  devExt->SupportsIndirect = (negotiated & AEROVBLK_FEATURE_RING_INDIRECT_DESC) ? TRUE : FALSE;
  devExt->SupportsFlush = (negotiated & AEROVBLK_FEATURE_BLK_FLUSH) ? TRUE : FALSE;

  RtlZeroMemory(&cfg, sizeof(cfg));
  st = AerovblkVirtioReadBlkConfig(devExt, &cfg);
  if (!NT_SUCCESS(st)) {
    cfg.Capacity = 0;
    cfg.BlkSize = 0;
    cfg.SegMax = 0;
  }

  devExt->CapacitySectors = cfg.Capacity;
  devExt->LogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
  if ((negotiated & AEROVBLK_FEATURE_BLK_BLK_SIZE) && cfg.BlkSize >= AEROVBLK_LOGICAL_SECTOR_SIZE &&
      (cfg.BlkSize % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0) {
    devExt->LogicalSectorSize = cfg.BlkSize;
  }

  devExt->SegMax = (cfg.SegMax != 0) ? cfg.SegMax : (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
  if (devExt->SegMax > (ULONG)AEROVBLK_MAX_SG_ELEMENTS) {
    devExt->SegMax = (ULONG)AEROVBLK_MAX_SG_ELEMENTS;
  }

  if (allocateResources) {
    if (!AerovblkAllocateVirtqueue(devExt)) {
      goto FailDevice;
    }

    if (!AerovblkAllocateRequestContexts(devExt)) {
      goto FailDevice;
    }
  } else {
    if (devExt->Vq.queue_size == 0 || devExt->RingDma.vaddr == NULL || devExt->RequestContexts == NULL) {
      goto FailDevice;
    }
  }

  queueSize = VirtioPciGetQueueSize(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX);
  if (queueSize != (USHORT)AEROVBLK_QUEUE_SIZE) {
    goto FailDevice;
  }

  // Contract v1: notify_off_multiplier=4 and queue_notify_off(q)=q.
  notifyAddr = NULL;
  st = VirtioPciGetQueueNotifyAddress(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX, &notifyAddr);
  if (!NT_SUCCESS(st) || notifyAddr == NULL) {
    goto FailDevice;
  }

  notifyOffset = (ULONGLONG)AEROVBLK_QUEUE_INDEX * (ULONGLONG)devExt->Vdev.NotifyOffMultiplier;
  expectedNotifyAddr = (volatile UINT16*)((volatile UCHAR*)devExt->Vdev.NotifyBase + notifyOffset);
  if (notifyAddr != expectedNotifyAddr) {
    goto FailDevice;
  }
  devExt->QueueNotifyAddrCache[0] = notifyAddr;

  /*
   * Contract v1 requires INTx and only permits MSI-X as an optional enhancement.
   * Disable (unassign) the queue MSI-X vector so the device must fall back to
   * INTx + ISR semantics even if MSI-X is present/enabled.
   */
  {
    KIRQL irql;

    KeAcquireSpinLock(&devExt->Vdev.CommonCfgLock, &irql);
    WRITE_REGISTER_USHORT((volatile USHORT*)&devExt->Vdev.CommonCfg->queue_select, (USHORT)AEROVBLK_QUEUE_INDEX);
    KeMemoryBarrier();
    WRITE_REGISTER_USHORT((volatile USHORT*)&devExt->Vdev.CommonCfg->queue_msix_vector, VIRTIO_PCI_MSI_NO_VECTOR);
    KeMemoryBarrier();
    KeReleaseSpinLock(&devExt->Vdev.CommonCfgLock, irql);
  }

  descPa = devExt->RingDma.paddr + (UINT64)((PUCHAR)devExt->Vq.desc - (PUCHAR)devExt->RingDma.vaddr);
  availPa = devExt->RingDma.paddr + (UINT64)((PUCHAR)devExt->Vq.avail - (PUCHAR)devExt->RingDma.vaddr);
  usedPa = devExt->RingDma.paddr + (UINT64)((PUCHAR)devExt->Vq.used - (PUCHAR)devExt->RingDma.vaddr);

  st = VirtioPciSetupQueue(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX, descPa, availPa, usedPa);
  if (!NT_SUCCESS(st)) {
    goto FailDevice;
  }

  VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_DRIVER_OK);

  StorPortNotification(NextRequest, devExt, NULL);
  return TRUE;

FailDevice:
  VirtioPciFailDevice(&devExt->Vdev);
  if (allocateResources) {
    /*
     * If bring-up fails after we've allocated DMA-backed resources, ensure the
     * device is reset before freeing memory it may DMA to (ring + indirect
     * tables + request context pages).
     */
    VirtioPciResetDevice(&devExt->Vdev);
    AerovblkFreeResources(devExt);
    /* Leave the device in FAILED for host visibility. */
    VirtioPciFailDevice(&devExt->Vdev);
  }
  return FALSE;
}

static BOOLEAN AerovblkQueueRequest(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ ULONG reqType,
                                   _In_ ULONGLONG startSector, _In_opt_ PSTOR_SCATTER_GATHER_LIST sg, _In_ BOOLEAN isWrite) {
  STOR_LOCK_HANDLE lock;
  LIST_ENTRY* entry;
  PAEROVBLK_REQUEST_CONTEXT ctx;
  ULONG sgCount;
  uint16_t totalDesc;
  uint16_t headId;
  ULONG i;
  virtio_sg_entry_t segs[AEROVBLK_MAX_SG_ELEMENTS + 2];
  int vqRes;
  virtio_bool_t useIndirect;

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  if (devExt->Removed) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkSetSense(devExt, srb, SCSI_SENSE_NOT_READY, 0x04, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    return TRUE;
  }

  if (devExt->Vq.queue_size == 0) {
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

  totalDesc = (uint16_t)(sgCount + 2u);

  segs[0].addr = (uint64_t)(ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_HDR_OFFSET);
  segs[0].len = (uint32_t)sizeof(VIRTIO_BLK_REQ_HDR);
  segs[0].device_writes = VIRTIO_FALSE;

  for (i = 0; i < sgCount; ++i) {
    segs[1 + i].addr = (uint64_t)sg->Elements[i].PhysicalAddress.QuadPart;
    segs[1 + i].len = (uint32_t)sg->Elements[i].Length;
    segs[1 + i].device_writes = isWrite ? VIRTIO_FALSE : VIRTIO_TRUE;
  }

  segs[1 + sgCount].addr = (uint64_t)(ctx->SharedPagePa.QuadPart + AEROVBLK_CTX_STATUS_OFFSET);
  segs[1 + sgCount].len = 1;
  segs[1 + sgCount].device_writes = VIRTIO_TRUE;

  useIndirect = (devExt->Vq.indirect_desc != VIRTIO_FALSE) ? VIRTIO_TRUE : VIRTIO_FALSE;
  headId = 0;
  vqRes = virtqueue_split_add_sg(&devExt->Vq, segs, totalDesc, ctx, useIndirect, &headId);
  if (vqRes != VIRTIO_OK) {
    ctx->Srb = NULL;
    InsertTailList(&devExt->FreeRequestList, &ctx->Link);
    devExt->FreeRequestCount++;
    StorPortReleaseSpinLock(devExt, &lock);

    if (vqRes == VIRTIO_ERR_NOSPC) {
      return FALSE;
    }

    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
    return TRUE;
  }

  /* Contract v1 requires always-notify semantics (EVENT_IDX not negotiated). */
  UNREFERENCED_PARAMETER(headId);
  KeMemoryBarrier();
  AerovblkVirtioNotifyQueue0(devExt);

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
  if (devExt->Vq.queue_size != 0 && devExt->Vq.used != NULL) {
    info->QueueSize = (USHORT)devExt->Vq.queue_size;
    info->NumFree = (USHORT)devExt->Vq.num_free;
    info->AvailIdx = (USHORT)devExt->Vq.avail_idx;
    info->UsedIdx = (USHORT)devExt->Vq.used->idx;
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
  UCHAR pciCfg[256];
  ULONG bytesRead;
  USHORT vendorId;
  USHORT deviceId;
  ULONG accessRangeIndex;
  USHORT hwQueueSize;
  volatile UINT16* notifyAddr;
  volatile UINT16* expectedNotifyAddr;
  ULONGLONG notifyOffset;
  ULONGLONG hostFeatures;
  ULONGLONG required;
  ULONG maxPhysBreaks;
  VIRTIO_BLK_CONFIG blkCfg;
  NTSTATUS st;
  ULONG alignment;
  ULONG maxTransfer;

  UNREFERENCED_PARAMETER(hwContext);
  UNREFERENCED_PARAMETER(busInformation);
  UNREFERENCED_PARAMETER(argumentString);

  *again = FALSE;

  if (configInfo->NumberOfAccessRanges < 1) {
    return SP_RETURN_NOT_FOUND;
  }

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  RtlZeroMemory(devExt, sizeof(*devExt));

  virtio_os_storport_get_ops(&devExt->VirtioOps);
  devExt->VirtioOpsCtx.pool_tag = 'bVrA';

  /*
   * Contract v1 binds to PCI Revision ID 0x01.
   * Read directly from PCI config space via StorPort bus data access.
   */
  RtlZeroMemory(pciCfg, sizeof(pciCfg));
  bytesRead = StorPortGetBusData(devExt, PCIConfiguration, configInfo->SystemIoBusNumber, configInfo->SlotNumber, pciCfg, sizeof(pciCfg));
  if (bytesRead != sizeof(pciCfg)) {
    return SP_RETURN_NOT_FOUND;
  }
  RtlCopyMemory(&vendorId, pciCfg + 0x00, sizeof(vendorId));
  RtlCopyMemory(&deviceId, pciCfg + 0x02, sizeof(deviceId));
  if (vendorId != (USHORT)AEROVBLK_PCI_VENDOR_ID || deviceId != (USHORT)AEROVBLK_PCI_DEVICE_ID ||
      pciCfg[0x08] != (UCHAR)AEROVBLK_VIRTIO_PCI_REVISION_ID) {
    return SP_RETURN_NOT_FOUND;
  }

  /* Contract v1: INTA# is required. */
  if (pciCfg[0x3D] != 0x01u) {
    return SP_RETURN_NOT_FOUND;
  }

  /*
   * Contract v1: BAR0 must be 64-bit MMIO and must match the mapped range.
   * Some platforms report multiple access ranges; do not assume BAR0 is at index 0.
   */
  {
    ULONG bar0Low;
    ULONG bar0High;
    ULONGLONG bar0Base;

    bar0Low = 0;
    bar0High = 0;
    RtlCopyMemory(&bar0Low, pciCfg + 0x10, sizeof(bar0Low));
    RtlCopyMemory(&bar0High, pciCfg + 0x14, sizeof(bar0High));

    if ((bar0Low & 0x1u) != 0) {
      return SP_RETURN_NOT_FOUND;
    }
    if ((bar0Low & 0x6u) != 0x4u) {
      return SP_RETURN_NOT_FOUND;
    }

    bar0Base = ((ULONGLONG)bar0High << 32) | (ULONGLONG)(bar0Low & ~0xFu);

    range = NULL;
    for (accessRangeIndex = 0; accessRangeIndex < configInfo->NumberOfAccessRanges; ++accessRangeIndex) {
      PACCESS_RANGE candidate;

      candidate = &configInfo->AccessRanges[accessRangeIndex];
      if (!candidate->RangeInMemory) {
        continue;
      }
      if (candidate->RangeLength < AEROVBLK_BAR0_MIN_LEN) {
        continue;
      }
      if ((ULONGLONG)candidate->RangeStart.QuadPart != bar0Base) {
        continue;
      }

      range = candidate;
      break;
    }

    if (range == NULL) {
      return SP_RETURN_NOT_FOUND;
    }
  }

  base = StorPortGetDeviceBase(devExt, configInfo->AdapterInterfaceType, configInfo->SystemIoBusNumber, range->RangeStart, range->RangeLength, FALSE);
  if (base == NULL) {
    return SP_RETURN_NOT_FOUND;
  }

  st = VirtioPciModernMiniportInit(&devExt->Vdev, (PUCHAR)base, range->RangeLength, pciCfg, sizeof(pciCfg));
  if (!NT_SUCCESS(st)) {
    return SP_RETURN_NOT_FOUND;
  }

  devExt->Vdev.QueueNotifyAddrCache = devExt->QueueNotifyAddrCache;
  devExt->Vdev.QueueNotifyAddrCacheCount = RTL_NUMBER_OF(devExt->QueueNotifyAddrCache);

  if (!AeroVirtioValidateContractV1Bar0Layout(&devExt->Vdev)) {
    return SP_RETURN_NOT_FOUND;
  }

  /* Validate queue 0 size (contract v1: 128). */
  hwQueueSize = VirtioPciGetQueueSize(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX);
  if (hwQueueSize != (USHORT)AEROVBLK_QUEUE_SIZE) {
    return SP_RETURN_NOT_FOUND;
  }

  notifyAddr = NULL;
  st = VirtioPciGetQueueNotifyAddress(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX, &notifyAddr);
  if (!NT_SUCCESS(st) || notifyAddr == NULL) {
    return SP_RETURN_NOT_FOUND;
  }

  notifyOffset = (ULONGLONG)AEROVBLK_QUEUE_INDEX * (ULONGLONG)devExt->Vdev.NotifyOffMultiplier;
  expectedNotifyAddr = (volatile UINT16*)((volatile UCHAR*)devExt->Vdev.NotifyBase + notifyOffset);
  if (notifyAddr != expectedNotifyAddr) {
    return SP_RETURN_NOT_FOUND;
  }
  devExt->QueueNotifyAddrCache[0] = notifyAddr;

  /* Validate required features are offered (contract v1). */
  hostFeatures = VirtioPciReadDeviceFeatures(&devExt->Vdev);
  required = VIRTIO_F_VERSION_1 | AEROVBLK_FEATURE_RING_INDIRECT_DESC | AEROVBLK_FEATURE_BLK_SEG_MAX | AEROVBLK_FEATURE_BLK_BLK_SIZE |
             AEROVBLK_FEATURE_BLK_FLUSH;
  if ((hostFeatures & required) != required) {
    return SP_RETURN_NOT_FOUND;
  }

  RtlZeroMemory(&blkCfg, sizeof(blkCfg));
  st = AerovblkVirtioReadBlkConfig(devExt, &blkCfg);
  if (!NT_SUCCESS(st)) {
    blkCfg.Capacity = 0;
    blkCfg.BlkSize = 0;
    blkCfg.SegMax = 0;
  }

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

    /*
     * Stop the device before aborting in-flight requests to prevent the device
     * from continuing DMA while we tear down the queue.
     */
    if (devExt->Vdev.CommonCfg != NULL) {
      VirtioPciResetDevice(&devExt->Vdev);
    }

    if (controlType == ScsiStopAdapter) {
      StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
      AerovblkAbortOutstandingRequestsLocked(devExt);
      if (devExt->Vq.queue_size != 0) {
        AerovblkResetVirtqueueLocked(devExt);
      }
      StorPortReleaseSpinLock(devExt, &lock);
      return ScsiAdapterControlSuccess;
    }

    /*
     * ScsiRemoveAdapter is the final teardown path (driver unload / hot-remove).
     * Abort outstanding requests and release all allocations.
     */
    AerovblkFreeResources(devExt);
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
  uint32_t usedLen;
  PAEROVBLK_REQUEST_CONTEXT ctx;
  PSCSI_REQUEST_BLOCK srb;
  UCHAR statusByte;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

  /*
   * Modern virtio-pci ISR byte (BAR0 + 0x2000). Read-to-ack.
   * Return FALSE if 0 for shared interrupt line safety.
   */
  isr = VirtioPciReadIsr(&devExt->Vdev);
  if (isr == 0) {
    return FALSE;
  }

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  if (devExt->Vq.queue_size != 0) {
    for (;;) {
      ctxPtr = NULL;
      if (virtqueue_split_pop_used(&devExt->Vq, &ctxPtr, &usedLen) == VIRTIO_FALSE) {
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

  if (srb->Function == SRB_FUNCTION_FLUSH || srb->Function == SRB_FUNCTION_SHUTDOWN) {
    /*
     * StorPort may issue cache flushes via SRB function codes rather than
     * SCSI CDBs (SCSIOP_SYNCHRONIZE_CACHE*). Ensure we translate those into a
     * virtio-blk flush request when supported. If flush is not supported, treat
     * as a no-op per StorPort expectations.
     *
     * On resource exhaustion (no free request context / virtqueue full),
     * AerovblkQueueRequest returns FALSE and the SRB is left pending so StorPort
     * can retry/requeue.
     */
    if (!devExt->SupportsFlush) {
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
      return TRUE;
    }

    return AerovblkQueueRequest(devExt, srb, VIRTIO_BLK_T_FLUSH, 0, NULL, FALSE);
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

  case SCSIOP_REPORT_LUNS: {
    ULONG allocLen;
    ULONG outLen;
    UCHAR resp[16];

    /* REPORT LUNS (12-byte CDB): allocation length is bytes 6..9 (big-endian). */
    allocLen = AerovblkBe32ToCpu(&srb->Cdb[6]);

    if (srb->DataBuffer == NULL || srb->DataTransferLength == 0 || allocLen == 0) {
      srb->DataTransferLength = 0;
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
      return TRUE;
    }

    outLen = (srb->DataTransferLength < allocLen) ? srb->DataTransferLength : allocLen;
    if (outLen > sizeof(resp)) {
      outLen = sizeof(resp);
    }

    /*
     * Minimal REPORT LUNS response for one LUN (LUN0):
     *   - LUN list length: 8 (big-endian)
     *   - reserved: 0
     *   - one 8-byte LUN entry: all zeros
     */
    RtlZeroMemory(resp, sizeof(resp));
    AerovblkWriteBe32(resp + 0, 8u);

    RtlCopyMemory(srb->DataBuffer, resp, outLen);
    srb->DataTransferLength = outLen;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
  }

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
      /*
       * SCSI READ/WRITE(10): transfer length of 0 means no data transfer.
       * Complete successfully without issuing any device I/O.
       */
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
