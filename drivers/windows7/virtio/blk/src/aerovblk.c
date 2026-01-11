#include "../include/aerovblk.h"

static UINT8 AerovblkTransportPciRead8(void *context, UINT16 offset)
{
	PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)context;

	if (devExt == NULL || offset >= (UINT16)sizeof(devExt->PciCfgSpace)) {
		return 0;
	}
	return (UINT8)devExt->PciCfgSpace[offset];
}

static UINT16 AerovblkTransportPciRead16(void *context, UINT16 offset)
{
	PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)context;

	if (devExt == NULL || (UINT32)offset + sizeof(UINT16) > sizeof(devExt->PciCfgSpace)) {
		return 0;
	}
	return (UINT16)devExt->PciCfgSpace[offset] | ((UINT16)devExt->PciCfgSpace[offset + 1] << 8);
}

static UINT32 AerovblkTransportPciRead32(void *context, UINT16 offset)
{
	PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)context;

	if (devExt == NULL || (UINT32)offset + sizeof(UINT32) > sizeof(devExt->PciCfgSpace)) {
		return 0;
	}
	return (UINT32)devExt->PciCfgSpace[offset] | ((UINT32)devExt->PciCfgSpace[offset + 1] << 8) |
	       ((UINT32)devExt->PciCfgSpace[offset + 2] << 16) | ((UINT32)devExt->PciCfgSpace[offset + 3] << 24);
}

static NTSTATUS AerovblkTransportMapMmio(void *context, UINT64 physicalAddress, UINT32 length, volatile void **mappedVaOut)
{
	PAEROVBLK_DEVICE_EXTENSION devExt = (PAEROVBLK_DEVICE_EXTENSION)context;
	STOR_PHYSICAL_ADDRESS pa;
	PVOID va;

	if (mappedVaOut != NULL) {
		*mappedVaOut = NULL;
	}

	if (devExt == NULL || mappedVaOut == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	pa.QuadPart = physicalAddress;
	va = StorPortGetDeviceBase(devExt, devExt->PciInterfaceType, devExt->PciBusNumber, pa, length, FALSE);
	if (va == NULL) {
		return STATUS_INSUFFICIENT_RESOURCES;
	}

	*mappedVaOut = (volatile void *)va;
	return STATUS_SUCCESS;
}

static void AerovblkTransportUnmapMmio(void *context, volatile void *mappedVa, UINT32 length)
{
	UNREFERENCED_PARAMETER(context);
	UNREFERENCED_PARAMETER(mappedVa);
	UNREFERENCED_PARAMETER(length);
	/* StorPort does not require explicit unmap. */
}

static void AerovblkTransportStallUs(void *context, UINT32 microseconds)
{
	UNREFERENCED_PARAMETER(context);
	KeStallExecutionProcessor(microseconds);
}

static void AerovblkTransportMemoryBarrier(void *context)
{
	UNREFERENCED_PARAMETER(context);
	KeMemoryBarrier();
}

static void *AerovblkTransportSpinlockCreate(void *context)
{
	UNREFERENCED_PARAMETER(context);

	{
		KSPIN_LOCK *lock = (KSPIN_LOCK *)ExAllocatePoolWithTag(NonPagedPool, sizeof(KSPIN_LOCK), 'bVrA');
		if (lock == NULL) {
			return NULL;
		}
		KeInitializeSpinLock(lock);
		return lock;
	}
}

static void AerovblkTransportSpinlockDestroy(void *context, void *lock)
{
	UNREFERENCED_PARAMETER(context);
	if (lock != NULL) {
		ExFreePoolWithTag(lock, 'bVrA');
	}
}

static void AerovblkTransportSpinlockAcquire(void *context, void *lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE *stateOut)
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

static void AerovblkTransportSpinlockRelease(void *context, void *lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE state)
{
	UNREFERENCED_PARAMETER(context);

	if (lock == NULL) {
		return;
	}

	KeReleaseSpinLock((PKSPIN_LOCK)lock, (KIRQL)state);
}

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

static NTSTATUS AerovblkVirtioReadBlkConfig(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt, _Out_ PVIRTIO_BLK_CONFIG cfg) {
  if (cfg == NULL) {
    return STATUS_INVALID_PARAMETER;
  }

  RtlZeroMemory(cfg, sizeof(*cfg));
  return VirtioPciModernTransportReadDeviceConfig(&devExt->Transport, 0, cfg, sizeof(*cfg));
}

static VOID AerovblkVirtioNotifyQueue0(_Inout_ PAEROVBLK_DEVICE_EXTENSION devExt) {
  (void)VirtioPciModernTransportNotifyQueue(&devExt->Transport, (USHORT)AEROVBLK_QUEUE_INDEX);
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
  ULONGLONG requiredFeatures;
  ULONGLONG negotiated;
  VIRTIO_BLK_CONFIG cfg;
  NTSTATUS st;
  USHORT queueSize;
  USHORT notifyOff;

  if (devExt->Transport.CommonCfg == NULL || devExt->Transport.DeviceCfg == NULL) {
    return FALSE;
  }

  if (!allocateResources) {
    /*
     * Reset the device first to stop DMA before touching ring memory or
     * completing outstanding SRBs. This matches the legacy driver's sequencing
     * (reset before abort/reset of software queue state) and avoids races where
     * the device could still be writing used-ring entries while we recycle
     * request contexts.
     */
    VirtioPciModernTransportResetDevice(&devExt->Transport);

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
    AerovblkAbortOutstandingRequestsLocked(devExt);
    if (devExt->Vq != NULL) {
      VirtqSplitReset(devExt->Vq);
    }
    StorPortReleaseSpinLock(devExt, &lock);
  }

  requiredFeatures = AEROVBLK_FEATURE_RING_INDIRECT_DESC | AEROVBLK_FEATURE_BLK_SEG_MAX | AEROVBLK_FEATURE_BLK_BLK_SIZE | AEROVBLK_FEATURE_BLK_FLUSH;

  st = VirtioPciModernTransportNegotiateFeatures(&devExt->Transport, requiredFeatures, /*Wanted*/ 0, &negotiated);
  if (!NT_SUCCESS(st)) {
    return FALSE;
  }

  /* Disable MSI-X config vector (INTx required by contract v1). */
  (void)VirtioPciModernTransportSetConfigMsixVector(&devExt->Transport, 0xFFFFu);

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
      VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_FAILED);
      return FALSE;
    }

    if (!AerovblkAllocateRequestContexts(devExt)) {
      VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_FAILED);
      return FALSE;
    }
  } else {
    if (devExt->Vq == NULL || devExt->RequestContexts == NULL) {
      VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_FAILED);
      return FALSE;
    }
  }

  st = VirtioPciModernTransportGetQueueSize(&devExt->Transport, (USHORT)AEROVBLK_QUEUE_INDEX, &queueSize);
  if (!NT_SUCCESS(st) || queueSize != (USHORT)AEROVBLK_QUEUE_SIZE) {
    VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  notifyOff = 0;
  st = VirtioPciModernTransportGetQueueNotifyOff(&devExt->Transport, (USHORT)AEROVBLK_QUEUE_INDEX, &notifyOff);
  if (!NT_SUCCESS(st)) {
    VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  /*
   * Contract v1 requires INTx and only permits MSI-X as an optional enhancement.
   * Disable (unassign) the queue MSI-X vector so the device must fall back to
   * INTx + ISR semantics even if MSI-X is present/enabled.
   */
  st = VirtioPciModernTransportSetQueueMsixVector(&devExt->Transport, (USHORT)AEROVBLK_QUEUE_INDEX, 0xFFFFu);
  if (!NT_SUCCESS(st)) {
    VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  st = VirtioPciModernTransportSetupQueue(&devExt->Transport,
                                          (USHORT)AEROVBLK_QUEUE_INDEX,
                                          (ULONGLONG)devExt->Vq->desc_pa,
                                          (ULONGLONG)devExt->Vq->avail_pa,
                                          (ULONGLONG)devExt->Vq->used_pa);
  if (!NT_SUCCESS(st)) {
    VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_FAILED);
    return FALSE;
  }

  VirtioPciModernTransportAddStatus(&devExt->Transport, VIRTIO_STATUS_DRIVER_OK);

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
  /* Contract v1 requires always-notify semantics (EVENT_IDX not negotiated). */
  AerovblkVirtioNotifyQueue0(devExt);
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
  ULONG bytesRead;
  USHORT vendorId;
  USHORT deviceId;
  USHORT hwQueueSize;
  USHORT notifyOff;
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

  range = &configInfo->AccessRanges[0];
  if (!range->RangeInMemory) {
    return SP_RETURN_NOT_FOUND;
  }
  if (range->RangeLength < VIRTIO_PCI_MODERN_TRANSPORT_BAR0_REQUIRED_LEN) {
    return SP_RETURN_NOT_FOUND;
  }

  devExt = (PAEROVBLK_DEVICE_EXTENSION)deviceExtension;
  RtlZeroMemory(devExt, sizeof(*devExt));
  devExt->PciInterfaceType = configInfo->AdapterInterfaceType;
  devExt->PciBusNumber = configInfo->SystemIoBusNumber;
  devExt->PciSlotNumber = configInfo->SlotNumber;

  /*
   * Contract v1 binds to PCI Revision ID 0x01.
   * Read directly from PCI config space via StorPort bus data access.
   */
  bytesRead = StorPortGetBusData(devExt,
                                 PCIConfiguration,
                                 configInfo->SystemIoBusNumber,
                                 configInfo->SlotNumber,
                                 devExt->PciCfgSpace,
                                 sizeof(devExt->PciCfgSpace));
  if (bytesRead < sizeof(devExt->PciCfgSpace)) {
    return SP_RETURN_NOT_FOUND;
  }
  RtlCopyMemory(&vendorId, devExt->PciCfgSpace + 0x00, sizeof(vendorId));
  RtlCopyMemory(&deviceId, devExt->PciCfgSpace + 0x02, sizeof(deviceId));
  if (vendorId != (USHORT)AEROVBLK_PCI_VENDOR_ID || deviceId != (USHORT)AEROVBLK_PCI_DEVICE_ID ||
      devExt->PciCfgSpace[0x08] != (UCHAR)AEROVBLK_VIRTIO_PCI_REVISION_ID) {
    return SP_RETURN_NOT_FOUND;
  }

  RtlZeroMemory(&devExt->TransportOs, sizeof(devExt->TransportOs));
  devExt->TransportOs.Context = devExt;
  devExt->TransportOs.PciRead8 = AerovblkTransportPciRead8;
  devExt->TransportOs.PciRead16 = AerovblkTransportPciRead16;
  devExt->TransportOs.PciRead32 = AerovblkTransportPciRead32;
  devExt->TransportOs.MapMmio = AerovblkTransportMapMmio;
  devExt->TransportOs.UnmapMmio = AerovblkTransportUnmapMmio;
  devExt->TransportOs.StallUs = AerovblkTransportStallUs;
  devExt->TransportOs.MemoryBarrier = AerovblkTransportMemoryBarrier;
  devExt->TransportOs.SpinlockCreate = AerovblkTransportSpinlockCreate;
  devExt->TransportOs.SpinlockDestroy = AerovblkTransportSpinlockDestroy;
  devExt->TransportOs.SpinlockAcquire = AerovblkTransportSpinlockAcquire;
  devExt->TransportOs.SpinlockRelease = AerovblkTransportSpinlockRelease;

  st = VirtioPciModernTransportInit(&devExt->Transport,
                                    &devExt->TransportOs,
                                    VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT,
                                    (UINT64)range->RangeStart.QuadPart,
                                    range->RangeLength);
  if (!NT_SUCCESS(st)) {
    VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR err = devExt->Transport.InitError;
    if (err == VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH || err == VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_64BIT_MMIO ||
        err == VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL) {
      VirtioPciModernTransportUninit(&devExt->Transport);
      st = VirtioPciModernTransportInit(&devExt->Transport,
                                        &devExt->TransportOs,
                                        VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT,
                                        (UINT64)range->RangeStart.QuadPart,
                                        range->RangeLength);
    }
  }
  if (!NT_SUCCESS(st)) {
    return SP_RETURN_NOT_FOUND;
  }

  /* Validate queue 0 size (contract v1: 128). */
  st = VirtioPciModernTransportGetQueueSize(&devExt->Transport, (USHORT)AEROVBLK_QUEUE_INDEX, &hwQueueSize);
  if (!NT_SUCCESS(st) || hwQueueSize != (USHORT)AEROVBLK_QUEUE_SIZE) {
    VirtioPciModernTransportUninit(&devExt->Transport);
    return SP_RETURN_NOT_FOUND;
  }
  notifyOff = 0;
  st = VirtioPciModernTransportGetQueueNotifyOff(&devExt->Transport, (USHORT)AEROVBLK_QUEUE_INDEX, &notifyOff);
  if (!NT_SUCCESS(st)) {
    VirtioPciModernTransportUninit(&devExt->Transport);
    return SP_RETURN_NOT_FOUND;
  }

  /* Validate required features are offered (contract v1). */
  hostFeatures = VirtioPciModernTransportReadDeviceFeatures(&devExt->Transport);
  required = VIRTIO_F_VERSION_1 | AEROVBLK_FEATURE_RING_INDIRECT_DESC | AEROVBLK_FEATURE_BLK_SEG_MAX | AEROVBLK_FEATURE_BLK_BLK_SIZE |
             AEROVBLK_FEATURE_BLK_FLUSH;
  if ((hostFeatures & required) != required) {
    VirtioPciModernTransportUninit(&devExt->Transport);
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

    StorPortAcquireSpinLock(devExt, InterruptLock, &lock);
    AerovblkAbortOutstandingRequestsLocked(devExt);
    if (devExt->Vq != NULL) {
      VirtqSplitReset(devExt->Vq);
    }
    StorPortReleaseSpinLock(devExt, &lock);

    if (devExt->Transport.CommonCfg != NULL) {
      VirtioPciModernTransportResetDevice(&devExt->Transport);
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

  /*
   * Modern virtio-pci ISR byte (BAR0 + 0x2000). Read-to-ack.
   * Return FALSE if 0 for shared interrupt line safety.
   */
  isr = VirtioPciModernTransportReadIsrStatus(&devExt->Transport);
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
