#include "../include/aero_virtio_blk.h"

#include "virtio_pci_aero_layout_miniport.h"

#define VIRTIO_PCI_ISR_QUEUE_INTERRUPT  0x01u
#define VIRTIO_PCI_ISR_CONFIG_INTERRUPT 0x02u

static VOID AerovblkCompleteSrb(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb, _In_ UCHAR srbStatus);

static VOID AerovblkCaptureInterruptMode(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  PIO_INTERRUPT_MESSAGE_INFO msgInfo;
  ULONG msgCount;

  if (devExt == NULL) {
    return;
  }

  devExt->UseMsi = FALSE;
  devExt->MsiMessageCount = 0;
  devExt->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
  devExt->MsixQueue0Vector = VIRTIO_PCI_MSI_NO_VECTOR;

  /*
   * StorPort exposes message-signaled interrupt assignments via
   * StorPortGetMessageInterruptInformation(). When the device is configured for
   * MSI/MSI-X, this returns an IO_INTERRUPT_MESSAGE_INFO describing the
   * connected message interrupts, including MessageCount.
   *
   * When running on INTx, the call returns NULL (or a structure with
   * MessageCount==0 depending on WDK/OS version). Treat both as INTx.
   */
  msgInfo = StorPortGetMessageInterruptInformation(devExt);
  if (msgInfo == NULL) {
    return;
  }

  msgCount = msgInfo->MessageCount;
  if (msgCount == 0) {
    return;
  }

  if (msgCount > 0xFFFFu) {
    msgCount = 0xFFFFu;
  }

  devExt->UseMsi = TRUE;
  devExt->MsiMessageCount = (USHORT)msgCount;
#if DBG
  AEROVBLK_LOG("message interrupts assigned: messages=%hu", (USHORT)msgCount);
#endif
}

static BOOLEAN AerovblkProgramMsixVectors(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  USHORT configVec;
  USHORT queueVec;
  NTSTATUS st;

  if (devExt == NULL || devExt->Vdev.CommonCfg == NULL) {
    return FALSE;
  }

  if (!devExt->UseMsi) {
    /*
     * INTx path: ensure MSI-X vectors are unassigned.
     *
     * On Aero contract devices:
     * - If MSI-X is disabled at the PCI layer (INTx resources), the device delivers interrupts
     *   via INTx + ISR semantics.
     * - If MSI-X is enabled, `VIRTIO_PCI_MSI_NO_VECTOR` suppresses interrupts for that source
     *   (no MSI-X message and no INTx fallback).
     */
    devExt->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    devExt->MsixQueue0Vector = VIRTIO_PCI_MSI_NO_VECTOR;
    (void)VirtioPciDisableMsixVectors(&devExt->Vdev, /*QueueCount=*/1);
    return TRUE;
  }

  /*
   * MSI/MSI-X path:
   *  - config vector: 0
   *  - queue0 vector: 1 if we have >= 2 messages, else share vector 0.
   *
   * The message IDs that StorPort delivers to HwMSInterruptRoutine map to the
   * MSI-X table entry indices that virtio expects in msix_config /
   * queue_msix_vector.
   */
  configVec = 0;
  queueVec = (devExt->MsiMessageCount >= 2u) ? 1u : 0u;

  st = VirtioPciSetConfigMsixVector(&devExt->Vdev, configVec);
  if (NT_SUCCESS(st)) {
    st = VirtioPciSetQueueMsixVector(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX, queueVec);
    if (!NT_SUCCESS(st) && queueVec != configVec) {
      /* Fallback: route queue interrupts to vector 0 as well. */
      queueVec = configVec;
      st = VirtioPciSetQueueMsixVector(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX, queueVec);
    }
  }

  if (NT_SUCCESS(st)) {
    devExt->MsixConfigVector = configVec;
    devExt->MsixQueue0Vector = queueVec;
#if DBG
    AEROVBLK_LOG("msix routing ok: messages=%hu config=%hu queue0=%hu", (USHORT)devExt->MsiMessageCount, (USHORT)devExt->MsixConfigVector,
                 (USHORT)devExt->MsixQueue0Vector);
#endif
    return TRUE;
  }

  /*
   * Vector programming failed (readback NO_VECTOR): fall back to INTx.
   *
   * Contract v1 requires INTx correctness; MSI/MSI-X is an optional enhancement.
   */
  devExt->UseMsi = FALSE;
  devExt->MsiMessageCount = 0;
  devExt->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
  devExt->MsixQueue0Vector = VIRTIO_PCI_MSI_NO_VECTOR;
#if DBG
  AEROVBLK_LOG("msix routing failed st=0x%08lx; falling back to INTx", (unsigned long)st);
#endif
  (void)VirtioPciDisableMsixVectors(&devExt->Vdev, /*QueueCount=*/1);
  return TRUE;
}

static __forceinline virtio_bool_t AerovblkVirtqueueKickPrepareContractV1(_Inout_ virtqueue_split_t* Vq) {
  /*
   * Contract v1 devices do not require EVENT_IDX and some may not offer it, so
   * the default behaviour remains "always notify" for compatibility.
   *
   * If EVENT_IDX is negotiated, use the standard virtio notification suppression
   * algorithm via virtqueue_split_kick_prepare().
   */
  if (Vq == NULL) {
    return VIRTIO_FALSE;
  }

  if (Vq->avail_idx == Vq->last_kick_avail) {
    return VIRTIO_FALSE;
  }

  if (Vq->event_idx != VIRTIO_FALSE) {
    return virtqueue_split_kick_prepare(Vq);
  }

  /* Keep virtqueue bookkeeping consistent even when always-notify is used. */
  Vq->last_kick_avail = Vq->avail_idx;
  return VIRTIO_TRUE;
}

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

static __forceinline ULONGLONG AerovblkReadCapacitySectors(_In_ const PAEROVBLK_DEVICE_EXTENSION devExt) {
  if (devExt == NULL) {
    return 0;
  }
  return (ULONGLONG)InterlockedCompareExchange64((volatile LONGLONG*)&devExt->CapacitySectors, 0, 0);
}

static __forceinline VOID AerovblkWriteCapacitySectors(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _In_ ULONGLONG sectors) {
  if (devExt == NULL) {
    return;
  }
  (VOID)InterlockedExchange64((volatile LONGLONG*)&devExt->CapacitySectors, (LONGLONG)sectors);
}

static __forceinline ULONGLONG AerovblkReadCapacityChangeEvents(_In_ const PAEROVBLK_DEVICE_EXTENSION devExt) {
  if (devExt == NULL) {
    return 0;
  }
  return (ULONGLONG)InterlockedCompareExchange64((volatile LONGLONG*)&devExt->CapacityChangeEvents, 0, 0);
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
  ULONGLONG capacitySectors;
  ULONG logicalSectorSize;

  if (devExt == NULL) {
    return 0;
  }

  logicalSectorSize = devExt->LogicalSectorSize;
  if (logicalSectorSize == 0) {
    return 0;
  }

  capacitySectors = AerovblkReadCapacitySectors(devExt);
  capBytes = capacitySectors * (ULONGLONG)AEROVBLK_LOGICAL_SECTOR_SIZE;
  return capBytes / (ULONGLONG)logicalSectorSize;
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
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ABORTED | SRB_STATUS_AUTOSENSE_VALID);
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

  /*
   * Clear cached queue notify addresses so any late-path code in the VirtioPci
   * layer cannot use stale cached pointers after teardown.
   */
  devExt->QueueNotifyAddrCache[0] = NULL;

  StorPortReleaseSpinLock(devExt, &lock);

  AerovblkFreeRequestContextsArray(devExt, requestContexts, requestContextCount);

  /*
   * Destroy the virtqueue (frees cookies + indirect tables) and free the split
   * ring DMA buffer allocated via virtqueue_split_alloc_ring.
   */
  virtqueue_split_destroy(&vq);
  virtqueue_split_free_ring(&devExt->VirtioOps, &devExt->VirtioOpsCtx, &ringDma);
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

static VOID AerovblkHandleConfigInterrupt(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  VIRTIO_BLK_CONFIG cfg;
  NTSTATUS st;
  ULONGLONG newCapacitySectors;
  ULONG newLogicalSectorSize;
  BOOLEAN changed;
  STOR_LOCK_HANDLE lock;
  UCHAR gen;

  if (devExt == NULL) {
    return;
  }

  if (devExt->Removed) {
    return;
  }

  if (devExt->ResetInProgress != 0) {
    return;
  }

  if (devExt->Vdev.CommonCfg == NULL || devExt->Vdev.DeviceCfg == NULL) {
    return;
  }

  /*
   * Config-change interrupts are keyed by `config_generation` (virtio-pci modern).
   * When MSI-X vectors are shared (e.g. only one message was granted), queue
   * interrupts may arrive on the same message ID as config interrupts; avoid an
   * expensive config read unless the generation has actually changed.
   */
  gen = READ_REGISTER_UCHAR((volatile UCHAR*)&devExt->Vdev.CommonCfg->config_generation);
  if (gen == devExt->LastConfigGeneration) {
    return;
  }

  RtlZeroMemory(&cfg, sizeof(cfg));
  st = AerovblkVirtioReadBlkConfig(devExt, &cfg);
  if (!NT_SUCCESS(st)) {
    return;
  }

  newCapacitySectors = cfg.Capacity;
  newLogicalSectorSize = AEROVBLK_LOGICAL_SECTOR_SIZE;
  if ((devExt->NegotiatedFeatures & AEROVBLK_FEATURE_BLK_BLK_SIZE) && cfg.BlkSize >= AEROVBLK_LOGICAL_SECTOR_SIZE &&
      (cfg.BlkSize % AEROVBLK_LOGICAL_SECTOR_SIZE) == 0) {
    newLogicalSectorSize = cfg.BlkSize;
  }

  changed = FALSE;

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  if (!devExt->Removed) {
    const ULONGLONG oldCapacitySectors = AerovblkReadCapacitySectors(devExt);
    const ULONG oldLogicalSectorSize = devExt->LogicalSectorSize;

    /* Record the generation we handled so we can skip redundant config checks. */
    devExt->LastConfigGeneration = gen;

    if (newCapacitySectors != oldCapacitySectors || newLogicalSectorSize != oldLogicalSectorSize) {
      /*
       * Best-effort support for device models that resize the disk at runtime.
       * Update geometry under the interrupt lock so StartIo/queueing observes a
       * consistent capacity when validating I/O bounds.
       */
      devExt->LogicalSectorSize = newLogicalSectorSize;
      AerovblkWriteCapacitySectors(devExt, newCapacitySectors);
      (VOID)InterlockedIncrement64((volatile LONGLONG*)&devExt->CapacityChangeEvents);
      changed = TRUE;
    }
  }

  StorPortReleaseSpinLock(devExt, &lock);

  if (changed) {
    /*
     * Notify StorPort / class drivers that something about the target has
     * changed. This encourages a rescan/re-read of disk capacity.
     */
    StorPortNotification(BusChangeDetected, devExt, 0);
  }
}

static BOOLEAN AerovblkAllocateVirtqueue(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  int vqRes;
  uint16_t indirectMaxDesc;
  virtio_bool_t eventIdx;

  if (devExt == NULL) {
    return FALSE;
  }

  if (devExt->Vq.queue_size != 0) {
    return TRUE;
  }

  if (!devExt->SupportsIndirect) {
    return FALSE;
  }

  eventIdx = (devExt->NegotiatedFeatures & AEROVBLK_FEATURE_RING_EVENT_IDX) ? VIRTIO_TRUE : VIRTIO_FALSE;
  vqRes = virtqueue_split_alloc_ring(&devExt->VirtioOps,
                                     &devExt->VirtioOpsCtx,
                                     (uint16_t)AEROVBLK_QUEUE_SIZE,
                                     16,
                                     eventIdx,
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
                               eventIdx,
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
  UINT64 wantedFeatures;
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

  /*
   * Prevent concurrent reset/reinit attempts. StorPort can issue multiple
   * management SRBs (abort/reset) back-to-back; treat redundant bring-up calls
   * as a no-op success while a reset is already in progress.
   */
  if (InterlockedCompareExchange(&devExt->ResetInProgress, 1, 0) != 0) {
    return TRUE;
  }
  /* Refresh whether StorPort assigned message-signaled interrupts (MSI/MSI-X). */
  AerovblkCaptureInterruptMode(devExt);

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
    /*
     * Best-effort: clear virtio MSI-X vector routing before reset so we don't
     * receive message interrupts for vectors that are about to be torn down /
     * reprogrammed.
     */
    (void)VirtioPciDisableMsixVectors(&devExt->Vdev, /*QueueCount=*/1);
    VirtioPciResetDevice(&devExt->Vdev);

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
    AerovblkAbortOutstandingRequestsLocked(devExt);
    if (devExt->Vq.queue_size != 0) {
      AerovblkResetVirtqueueLocked(devExt);
    }
    StorPortReleaseSpinLock(devExt, &lock);
  }

  requiredFeatures = AEROVBLK_FEATURE_RING_INDIRECT_DESC | AEROVBLK_FEATURE_BLK_SEG_MAX | AEROVBLK_FEATURE_BLK_BLK_SIZE | AEROVBLK_FEATURE_BLK_FLUSH;

  /*
   * EVENT_IDX is an optional improvement: only request it when we can size the
   * ring accordingly.
   *
   * - Initial bring-up (allocateResources=TRUE): we can allocate an EVENT_IDX
   *   ring if the feature is negotiated.
   * - Reset/restart (allocateResources=FALSE): only renegotiate EVENT_IDX if the
   *   existing queue was created with it (ring layout is fixed).
   */
  wantedFeatures = 0;
  if (allocateResources || devExt->Vq.event_idx != VIRTIO_FALSE) {
    wantedFeatures |= AEROVBLK_FEATURE_RING_EVENT_IDX;
  }

  st = VirtioPciNegotiateFeatures(&devExt->Vdev, requiredFeatures, wantedFeatures, &negotiated);
  if (!NT_SUCCESS(st)) {
    InterlockedExchange(&devExt->ResetInProgress, 0);
    return FALSE;
  }

  if (!AerovblkProgramMsixVectors(devExt)) {
    goto FailDevice;
  }

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

  AerovblkWriteCapacitySectors(devExt, cfg.Capacity);
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

  descPa = devExt->RingDma.paddr + (UINT64)((PUCHAR)devExt->Vq.desc - (PUCHAR)devExt->RingDma.vaddr);
  availPa = devExt->RingDma.paddr + (UINT64)((PUCHAR)devExt->Vq.avail - (PUCHAR)devExt->RingDma.vaddr);
  usedPa = devExt->RingDma.paddr + (UINT64)((PUCHAR)devExt->Vq.used - (PUCHAR)devExt->RingDma.vaddr);

  st = VirtioPciSetupQueue(&devExt->Vdev, (USHORT)AEROVBLK_QUEUE_INDEX, descPa, availPa, usedPa);
  if (!NT_SUCCESS(st)) {
    goto FailDevice;
  }

  VirtioPciAddStatus(&devExt->Vdev, VIRTIO_STATUS_DRIVER_OK);

  /*
   * Seed config-generation tracking so MSI/MSI-X shared-vector paths can cheaply
   * detect real config changes without re-reading the device config on every
   * interrupt.
   */
  if (devExt->Vdev.CommonCfg != NULL) {
    devExt->LastConfigGeneration = READ_REGISTER_UCHAR((volatile UCHAR*)&devExt->Vdev.CommonCfg->config_generation);
  }

  InterlockedExchange(&devExt->ResetInProgress, 0);
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
  InterlockedExchange(&devExt->ResetInProgress, 0);
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
  virtio_bool_t needKick;

  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);

  if (devExt->Removed) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkSetSense(devExt, srb, SCSI_SENSE_NOT_READY, 0x04, 0x00);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
    return TRUE;
  }

  if (devExt->ResetInProgress != 0) {
    StorPortReleaseSpinLock(devExt, &lock);
    return FALSE;
  }

  if (devExt->Vq.queue_size == 0) {
    StorPortReleaseSpinLock(devExt, &lock);
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
    return TRUE;
  }

  /*
   * Capacity may change at runtime if the device model triggers a virtio config
   * change interrupt. Perform a final bounds check under the interrupt lock so
   * no out-of-range I/O is queued after a resize event.
   */
  if (reqType == VIRTIO_BLK_T_IN || reqType == VIRTIO_BLK_T_OUT) {
    ULONGLONG capSectors;
    ULONGLONG sectorsLen;

    if ((srb->DataTransferLength % AEROVBLK_LOGICAL_SECTOR_SIZE) != 0) {
      StorPortReleaseSpinLock(devExt, &lock);
      AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x24, 0x00);
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST | SRB_STATUS_AUTOSENSE_VALID);
      return TRUE;
    }

    sectorsLen = (ULONGLONG)srb->DataTransferLength / (ULONGLONG)AEROVBLK_LOGICAL_SECTOR_SIZE;
    capSectors = AerovblkReadCapacitySectors(devExt);

    if (startSector + sectorsLen < startSector || startSector + sectorsLen > capSectors) {
      StorPortReleaseSpinLock(devExt, &lock);
      AerovblkSetSense(devExt, srb, SCSI_SENSE_ILLEGAL_REQUEST, 0x21, 0x00);
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID);
      return TRUE;
    }
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

  needKick = AerovblkVirtqueueKickPrepareContractV1(&devExt->Vq);

  /* Contract v1 defaults to always-notify, but EVENT_IDX uses suppression logic. */
  UNREFERENCED_PARAMETER(headId);
  if (needKick != VIRTIO_FALSE) {
    KeMemoryBarrier();
    AerovblkVirtioNotifyQueue0(devExt);
  }

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
  AEROVBLK_QUERY_INFO outInfo;
  ULONG maxPayloadLen;
  ULONG payloadLen;
  ULONG copyLen;
  USHORT msixConfig;
  USHORT msixQueue0;

  if (srb->DataBuffer == NULL || srb->DataTransferLength < sizeof(SRB_IO_CONTROL)) {
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
    return;
  }

  ctrl = (PSRB_IO_CONTROL)srb->DataBuffer;
  if (RtlCompareMemory(ctrl->Signature, AEROVBLK_SRBIO_SIG, sizeof(ctrl->Signature)) != sizeof(ctrl->Signature)) {
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
    return;
  }

  if (ctrl->ControlCode == AEROVBLK_IOCTL_QUERY) {
    maxPayloadLen = srb->DataTransferLength - sizeof(SRB_IO_CONTROL);
    payloadLen = ctrl->Length;
    if (payloadLen > maxPayloadLen) {
      payloadLen = maxPayloadLen;
    }

    /*
     * Maintain backwards compatibility with callers that only understand the
     * original v1 layout (through UsedIdx). Callers can request/consume the first
     * 16 bytes and ignore the newer appended fields.
     */
    if (payloadLen < FIELD_OFFSET(AEROVBLK_QUERY_INFO, InterruptMode)) {
      ctrl->ReturnCode = (ULONG)STATUS_BUFFER_TOO_SMALL;
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
      return;
    }

    info = (PAEROVBLK_QUERY_INFO)((PUCHAR)srb->DataBuffer + sizeof(SRB_IO_CONTROL));

    RtlZeroMemory(&outInfo, sizeof(outInfo));
    outInfo.NegotiatedFeatures = devExt->NegotiatedFeatures;
    if (devExt->Vq.queue_size != 0 && devExt->Vq.used != NULL) {
      outInfo.QueueSize = (USHORT)devExt->Vq.queue_size;
      outInfo.NumFree = (USHORT)devExt->Vq.num_free;
      outInfo.AvailIdx = (USHORT)devExt->Vq.avail_idx;
      outInfo.UsedIdx = (USHORT)devExt->Vq.used->idx;
    } else {
      outInfo.QueueSize = 0;
      outInfo.NumFree = 0;
      outInfo.AvailIdx = 0;
      outInfo.UsedIdx = 0;
    }

    /*
     * Interrupt observability.
     *
     * Report the driver-selected interrupt mode (INTx vs MSI/MSI-X) as well as
     * the currently programmed virtio MSI-X vectors.
     */
    outInfo.InterruptMode = devExt->UseMsi ? AEROVBLK_INTERRUPT_MODE_MSI : AEROVBLK_INTERRUPT_MODE_INTX;
    outInfo.MessageCount = devExt->UseMsi ? (ULONG)devExt->MsiMessageCount : 0;
    outInfo.MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    outInfo.MsixQueue0Vector = VIRTIO_PCI_MSI_NO_VECTOR;
    outInfo.Reserved0 = 0;

    msixConfig = VIRTIO_PCI_MSI_NO_VECTOR;
    msixQueue0 = VIRTIO_PCI_MSI_NO_VECTOR;
    if (devExt->Vdev.CommonCfg != NULL) {
      KIRQL irql;

      msixConfig = READ_REGISTER_USHORT((volatile USHORT*)&devExt->Vdev.CommonCfg->msix_config);

      KeAcquireSpinLock(&devExt->Vdev.CommonCfgLock, &irql);
      WRITE_REGISTER_USHORT((volatile USHORT*)&devExt->Vdev.CommonCfg->queue_select, (USHORT)AEROVBLK_QUEUE_INDEX);
      KeMemoryBarrier();
      msixQueue0 = READ_REGISTER_USHORT((volatile USHORT*)&devExt->Vdev.CommonCfg->queue_msix_vector);
      KeMemoryBarrier();
      KeReleaseSpinLock(&devExt->Vdev.CommonCfgLock, irql);
    }

    outInfo.MsixConfigVector = msixConfig;
    outInfo.MsixQueue0Vector = msixQueue0;

    /* If vectors are assigned, treat the effective mode as MSI/MSI-X. */
    if (msixConfig != VIRTIO_PCI_MSI_NO_VECTOR || msixQueue0 != VIRTIO_PCI_MSI_NO_VECTOR) {
      outInfo.InterruptMode = AEROVBLK_INTERRUPT_MODE_MSI;
    }

    outInfo.AbortSrbCount = (ULONG)devExt->AbortSrbCount;
    outInfo.ResetDeviceSrbCount = (ULONG)devExt->ResetDeviceSrbCount;
    outInfo.ResetBusSrbCount = (ULONG)devExt->ResetBusSrbCount;
    outInfo.PnpSrbCount = (ULONG)devExt->PnpSrbCount;
    outInfo.IoctlResetCount = (ULONG)devExt->IoctlResetCount;
    outInfo.CapacityChangeEvents = (ULONG)AerovblkReadCapacityChangeEvents(devExt);

    copyLen = payloadLen;
    if (copyLen > sizeof(outInfo)) {
      copyLen = sizeof(outInfo);
    }
    RtlCopyMemory(info, &outInfo, copyLen);

    ctrl->ReturnCode = 0;
    ctrl->Length = copyLen;
    srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + copyLen;
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return;
  }

  if (ctrl->ControlCode == AEROVBLK_IOCTL_FORCE_RESET) {
#if !DBG
    /*
     * Debug-only stress path: disabled in free builds unless explicitly enabled
     * by recompiling with DBG=1.
     */
    ctrl->ReturnCode = (ULONG)STATUS_NOT_SUPPORTED;
    ctrl->Length = 0;
    srb->DataTransferLength = sizeof(SRB_IO_CONTROL);
    /*
     * Complete successfully so IOCTL_SCSI_MINIPORT callers can reliably inspect
     * SRB_IO_CONTROL.ReturnCode to detect that this debug hook is unavailable.
     */
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return;
#else
    InterlockedIncrement(&devExt->IoctlResetCount);

    ctrl->ReturnCode = 0;
    ctrl->Length = 0;
    srb->DataTransferLength = sizeof(SRB_IO_CONTROL);

    if (devExt->Removed) {
      /*
       * When the adapter is stopped/removed, do not attempt to reinitialize the
       * device. Treat this as a no-op success so a debug tool can probe the
       * interface without reviving the adapter.
       */
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
      return;
    }

    if (!AerovblkDeviceBringUp(devExt, FALSE)) {
      ctrl->ReturnCode = (ULONG)STATUS_UNSUCCESSFUL;
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
      return;
    }

    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return;
#endif
  }

  ctrl->ReturnCode = (ULONG)STATUS_NOT_SUPPORTED;
  AerovblkCompleteSrb(devExt, srb, SRB_STATUS_INVALID_REQUEST);
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
  initData.HwMSInterruptRoutine = AerovblkHwMSInterrupt;
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
  AerovblkWriteCapacitySectors(devExt, blkCfg.Capacity);
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

  /* Capture whether StorPort assigned message-signaled interrupts (MSI/MSI-X). */
  AerovblkCaptureInterruptMode(devExt);

  return SP_RETURN_FOUND;
}

BOOLEAN AerovblkHwInitialize(_In_ PVOID deviceExtension) {
  PAEROVBLK_DEVICE_EXTENSION devExt;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  AerovblkCaptureInterruptMode(devExt);
  return AerovblkDeviceBringUp(devExt, TRUE);
}

BOOLEAN AerovblkHwResetBus(_In_ PVOID deviceExtension, _In_ ULONG pathId) {
  PAEROVBLK_DEVICE_EXTENSION devExt;

  UNREFERENCED_PARAMETER(pathId);

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  if (devExt->Removed) {
    return TRUE;
  }
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
      /* Best-effort: clear virtio MSI-X vector routing before resetting/teardown. */
      (void)VirtioPciDisableMsixVectors(&devExt->Vdev, /*QueueCount=*/1);
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

static VOID AerovblkDrainCompletionsLocked(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  PVOID ctxPtr;
  uint32_t usedLen;
  PAEROVBLK_REQUEST_CONTEXT ctx;
  PSCSI_REQUEST_BLOCK srb;
  UCHAR statusByte;

  if (devExt == NULL) {
    return;
  }

  if (devExt->Vq.queue_size == 0) {
    return;
  }

  /*
   * When EVENT_IDX is negotiated, the device may suppress interrupts based on
   * the driver-written used_event field. Rearm it after draining completions.
   *
   * Mirror the standard virtqueue_enable_cb() pattern to avoid missing an
   * interrupt when the device produces new used entries while we are
   * re-enabling callbacks.
   */
  for (;;) {
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

    if (devExt->Vq.event_idx != VIRTIO_FALSE && devExt->Vq.used_event != NULL) {
      *((volatile uint16_t*)devExt->Vq.used_event) = devExt->Vq.last_used_idx;
      KeMemoryBarrier();

      if (devExt->Vq.used->idx == devExt->Vq.last_used_idx) {
        break;
      }

      continue;
    }

    break;
  }
}

static __forceinline BOOLEAN AerovblkServiceInterrupt(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  STOR_LOCK_HANDLE lock;
  BOOLEAN needReset;

  needReset = FALSE;
  StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
  if (devExt->ResetInProgress != 0 || devExt->Removed) {
    /*
     * Avoid draining the virtqueue or triggering new request dispatch while the
     * device/queue is being reset or the device is being stopped/removed.
     *
     * - The reset path will issue NextRequest once reinitialization is complete.
     * - Stop/remove paths abort outstanding requests and do not accept new I/O.
     */
    StorPortReleaseSpinLock(devExt, &lock);
    return TRUE;
  }
  AerovblkDrainCompletionsLocked(devExt);

  if (devExt->Vq.queue_size != 0) {
    const uint32_t vqErr = virtqueue_split_get_error_flags(&devExt->Vq);
    if (vqErr != 0) {
    /*
     * The virtqueue implementation detected invalid device behaviour (e.g.
     * corrupted used-ring entries). Ask StorPort to reset the bus so we can
     * reinitialize the device/queue and abort outstanding requests safely.
     */
      virtqueue_split_clear_error_flags(&devExt->Vq);
#if DBG
      AEROVBLK_LOG("virtqueue error_flags=0x%x; requesting ResetDetected", (unsigned)vqErr);
#endif
      needReset = TRUE;
    }
  }
  StorPortReleaseSpinLock(devExt, &lock);

  if (needReset) {
    StorPortNotification(ResetDetected, devExt, 0);
    return TRUE;
  }

  StorPortNotification(NextRequest, devExt, NULL);
  return TRUE;
}

BOOLEAN AerovblkHwInterrupt(_In_ PVOID deviceExtension) {
  PAEROVBLK_DEVICE_EXTENSION devExt;
  UCHAR isr;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  if (devExt == NULL || devExt->Removed) {
    /*
     * Avoid MMIO access after stop/remove (including surprise removal). If the
     * device is gone, reading the ISR byte may fault.
     */
    return FALSE;
  }

  /*
   * INTx path: modern virtio-pci ISR byte (BAR0 + 0x2000). Read-to-ack.
   * Return FALSE if 0 for shared interrupt line safety.
   */
  isr = VirtioPciReadIsr(&devExt->Vdev);
  if (isr == 0) {
    return FALSE;
  }

  if ((isr & VIRTIO_PCI_ISR_CONFIG_INTERRUPT) != 0) {
    AerovblkHandleConfigInterrupt(devExt);
  }

  return AerovblkServiceInterrupt(devExt);
}

BOOLEAN AerovblkHwMSInterrupt(_In_ PVOID deviceExtension, _In_ ULONG messageId) {
  PAEROVBLK_DEVICE_EXTENSION devExt;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  if (devExt == NULL || devExt->Removed) {
    /* Best-effort: ignore interrupts after stop/remove. */
    return TRUE;
  }

  /*
   * MSI/MSI-X interrupt semantics:
   * - There is no shared INTx line to ACK/deassert.
   * - Do NOT read the virtio ISR status byte here (read-to-ack is for INTx).
   *
   * virtio-blk (contract v1) uses one virtqueue (queue 0). We program config on
   * message 0 and queue 0 on message 1 when available, with fallback to sharing
   * message 0.
   *
   * When config and queue share a single message ID, we may see queue interrupts
   * on the config vector. AerovblkHandleConfigInterrupt() uses config_generation
   * to cheaply skip work unless the device actually changed config.
   */
  if (devExt->MsixConfigVector != VIRTIO_PCI_MSI_NO_VECTOR && messageId == (ULONG)devExt->MsixConfigVector) {
    AerovblkHandleConfigInterrupt(devExt);
  }

  return AerovblkServiceInterrupt(devExt);
}

BOOLEAN AerovblkHwStartIo(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb) {
  PAEROVBLK_DEVICE_EXTENSION devExt;
  UCHAR op;

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;

  /*
   * StorPort can issue management SRBs (abort/reset/PnP) with varying addressing
   * fields depending on the adapter stack. Handle these first, before enforcing
   * our single-LUN addressing model.
   */
  switch (srb->Function) {
  case SRB_FUNCTION_ABORT_COMMAND:
#ifdef SRB_FUNCTION_TERMINATE_IO
  /*
   * Some StorPort stacks use TERMINATE_IO rather than ABORT_COMMAND for timeout
   * recovery. Treat it equivalently.
   */
  case SRB_FUNCTION_TERMINATE_IO:
#endif
  {
    InterlockedIncrement(&devExt->AbortSrbCount);

    if (!devExt->Removed) {
      /*
       * We cannot reliably "cancel" a virtio-blk request without stopping DMA
       * because the virtqueue implementation does not support removing an
       * in-flight descriptor chain. Treat abort as a request to reset the
       * device/queue and complete all outstanding SRBs deterministically.
       */
      if (!AerovblkDeviceBringUp(devExt, FALSE)) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
        return TRUE;
      }
    } else {
      STOR_LOCK_HANDLE lock;
      StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
      AerovblkAbortOutstandingRequestsLocked(devExt);
      if (devExt->Vq.queue_size != 0) {
        AerovblkResetVirtqueueLocked(devExt);
      }
      StorPortReleaseSpinLock(devExt, &lock);
    }

    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
  }

#ifdef SRB_FUNCTION_FLUSH_QUEUE
  /*
   * Flush the adapter queue (error recovery). We treat this like ABORT_COMMAND:
   * stop DMA via reset, abort all outstanding SRBs deterministically, and
   * reinitialize the device/queue.
   */
  case SRB_FUNCTION_FLUSH_QUEUE: {
    if (!devExt->Removed) {
      if (!AerovblkDeviceBringUp(devExt, FALSE)) {
        AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
        return TRUE;
      }
    } else {
      STOR_LOCK_HANDLE lock;
      StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
      AerovblkAbortOutstandingRequestsLocked(devExt);
      if (devExt->Vq.queue_size != 0) {
        AerovblkResetVirtqueueLocked(devExt);
      }
      StorPortReleaseSpinLock(devExt, &lock);
    }

    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
  }
#endif

#ifdef SRB_FUNCTION_RELEASE_QUEUE
  /*
   * Queue release is a no-op for this driver because we do not implement an
   * internal frozen state machine; StorPort will resume dispatch naturally.
   */
  case SRB_FUNCTION_RELEASE_QUEUE:
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
#endif

#ifdef SRB_FUNCTION_LOCK_QUEUE
  /*
   * StorPort queue-freeze management SRBs. We do not maintain an internal
   * frozen state machine; StorPort will stop dispatching requests while the
   * queue is locked. Treat as a no-op success.
   */
  case SRB_FUNCTION_LOCK_QUEUE:
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
#endif

#ifdef SRB_FUNCTION_UNLOCK_QUEUE
  case SRB_FUNCTION_UNLOCK_QUEUE:
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
#endif

  case SRB_FUNCTION_RESET_DEVICE:
#ifdef SRB_FUNCTION_RESET_LOGICAL_UNIT
  /*
   * Treat LUN reset as a device reset since this miniport only exposes a
   * single LUN.
   */
  case SRB_FUNCTION_RESET_LOGICAL_UNIT:
#endif
  {
    InterlockedIncrement(&devExt->ResetDeviceSrbCount);
    if (!devExt->Removed && !AerovblkDeviceBringUp(devExt, FALSE)) {
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
      return TRUE;
    }
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
  }

  case SRB_FUNCTION_RESET_BUS: {
    InterlockedIncrement(&devExt->ResetBusSrbCount);
    if (!devExt->Removed && !AerovblkDeviceBringUp(devExt, FALSE)) {
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
      return TRUE;
    }
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
  }

#ifdef SRB_FUNCTION_RESET_ADAPTER
  /*
   * Some StorPort stacks issue RESET_ADAPTER rather than RESET_BUS. Treat it
   * as a bus reset for this miniport (single bus/device).
   */
  case SRB_FUNCTION_RESET_ADAPTER: {
    InterlockedIncrement(&devExt->ResetBusSrbCount);
    if (!devExt->Removed && !AerovblkDeviceBringUp(devExt, FALSE)) {
      AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
      return TRUE;
    }
    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
  }
#endif

  case SRB_FUNCTION_PNP: {
    /*
     * Basic PnP handling for real-world StorPort stacks.
     *
     * Most PnP actions are non-critical and can be treated as no-op success.
     * For stop/remove, ensure we stop DMA and abort outstanding I/O so the
     * storage class stack doesn't see timeouts during teardown.
     */
    PSCSI_PNP_REQUEST_BLOCK pnp;

    InterlockedIncrement(&devExt->PnpSrbCount);
    pnp = (PSCSI_PNP_REQUEST_BLOCK)srb->DataBuffer;
    if (pnp != NULL && srb->DataTransferLength >= sizeof(*pnp)) {
      if (pnp->PnPAction == StorStopDevice || pnp->PnPAction == StorRemoveDevice) {
        STOR_LOCK_HANDLE lock;

        /*
         * Mark removed under the interrupt lock so we don't race with the I/O
         * submission path (AerovblkQueueRequest).
         */
        StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
        devExt->Removed = TRUE;
        StorPortReleaseSpinLock(devExt, &lock);

        if (devExt->Vdev.CommonCfg != NULL) {
          (void)VirtioPciDisableMsixVectors(&devExt->Vdev, /*QueueCount=*/1);
          VirtioPciResetDevice(&devExt->Vdev);
        }

        StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
        AerovblkAbortOutstandingRequestsLocked(devExt);
        if (devExt->Vq.queue_size != 0) {
          AerovblkResetVirtqueueLocked(devExt);
        }
        StorPortReleaseSpinLock(devExt, &lock);
      } else if (pnp->PnPAction == StorStartDevice) {
        BOOLEAN allocateResources;
        STOR_LOCK_HANDLE lock;

        /* Clear removed under lock so StartIo/queue path sees consistent state. */
        StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
        devExt->Removed = FALSE;
        StorPortReleaseSpinLock(devExt, &lock);

        allocateResources = (devExt->Vq.queue_size == 0 || devExt->RequestContexts == NULL) ? TRUE : FALSE;
        if (!AerovblkDeviceBringUp(devExt, allocateResources)) {
          AerovblkCompleteSrb(devExt, srb, SRB_STATUS_ERROR);
          return TRUE;
        }
      }
    }

    AerovblkCompleteSrb(devExt, srb, SRB_STATUS_SUCCESS);
    return TRUE;
  }

  default:
    break;
  }

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

    if (virtioSector + sectorsLen > AerovblkReadCapacitySectors(devExt)) {
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

    if (virtioSector + sectorsLen > AerovblkReadCapacitySectors(devExt)) {
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
