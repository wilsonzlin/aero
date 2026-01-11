#include "../include/aerovnet.h"

#define AEROVNET_TAG 'tNvA'

static NDIS_HANDLE g_NdisDriverHandle = NULL;

static const NDIS_OID g_SupportedOids[] = {
    OID_GEN_SUPPORTED_LIST,
    OID_GEN_HARDWARE_STATUS,
    OID_GEN_MEDIA_SUPPORTED,
    OID_GEN_MEDIA_IN_USE,
    OID_GEN_PHYSICAL_MEDIUM,
    OID_GEN_MAXIMUM_FRAME_SIZE,
    OID_GEN_MAXIMUM_LOOKAHEAD,
    OID_GEN_CURRENT_LOOKAHEAD,
    OID_GEN_MAXIMUM_TOTAL_SIZE,
    OID_GEN_LINK_SPEED,
    OID_GEN_TRANSMIT_BLOCK_SIZE,
    OID_GEN_RECEIVE_BLOCK_SIZE,
    OID_GEN_VENDOR_ID,
    OID_GEN_VENDOR_DESCRIPTION,
    OID_GEN_DRIVER_VERSION,
    OID_GEN_VENDOR_DRIVER_VERSION,
    OID_GEN_MAC_OPTIONS,
    OID_GEN_MEDIA_CONNECT_STATUS,
    OID_GEN_CURRENT_PACKET_FILTER,
    OID_GEN_MAXIMUM_SEND_PACKETS,
    OID_GEN_XMIT_OK,
    OID_GEN_RCV_OK,
    OID_GEN_XMIT_ERROR,
    OID_GEN_RCV_ERROR,
    OID_GEN_RCV_NO_BUFFER,
    OID_GEN_LINK_STATE,
    OID_GEN_STATISTICS,
    OID_802_3_PERMANENT_ADDRESS,
    OID_802_3_CURRENT_ADDRESS,
    OID_802_3_MULTICAST_LIST,
    OID_802_3_MAXIMUM_LIST_SIZE,
};

// 1 Gbps default link speed.
static const ULONG64 g_DefaultLinkSpeedBps = 1000000000ull;

#define AEROVNET_MAX_TX_SG_ELEMENTS 32u

// OID_GEN_DRIVER_VERSION encoding is major in high byte, minor in low byte.
#define AEROVNET_OID_DRIVER_VERSION ((USHORT)((6u << 8) | 20u))

static __forceinline ULONG AerovNetSendCompleteFlagsForCurrentIrql(VOID) {
  return (KeGetCurrentIrql() == DISPATCH_LEVEL) ? NDIS_SEND_COMPLETE_FLAGS_DISPATCH_LEVEL : 0;
}

static __forceinline ULONG AerovNetReceiveIndicationFlagsForCurrentIrql(VOID) {
  return (KeGetCurrentIrql() == DISPATCH_LEVEL) ? NDIS_RECEIVE_FLAGS_DISPATCH_LEVEL : 0;
}

static VOID AerovNetFreeTxRequestNoLock(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_TX_REQUEST* TxReq) {
  TxReq->State = AerovNetTxFree;
  TxReq->Cancelled = FALSE;
  TxReq->Nbl = NULL;
  TxReq->Nb = NULL;
  TxReq->SgList = NULL;
  TxReq->DescHeadId = 0;
  InsertTailList(&Adapter->TxFreeList, &TxReq->Link);
}

static VOID AerovNetCompleteNblSend(_In_ AEROVNET_ADAPTER* Adapter, _Inout_ PNET_BUFFER_LIST Nbl, _In_ NDIS_STATUS Status) {
  NET_BUFFER_LIST_STATUS(Nbl) = Status;
  NdisMSendNetBufferListsComplete(Adapter->MiniportAdapterHandle, Nbl, AerovNetSendCompleteFlagsForCurrentIrql());
}

static VOID AerovNetTxNblCompleteOneNetBufferLocked(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PNET_BUFFER_LIST Nbl, _In_ NDIS_STATUS TxStatus,
                                                   _Inout_ PNET_BUFFER_LIST* CompleteNblHead, _Inout_ PNET_BUFFER_LIST* CompleteNblTail) {
  LONG Pending;
  NDIS_STATUS NblStatus;
  NDIS_STATUS FinalStatus;

  UNREFERENCED_PARAMETER(Adapter);

  // Record the first failure for the NBL.
  if (TxStatus != NDIS_STATUS_SUCCESS) {
    NblStatus = AEROVNET_NBL_GET_STATUS(Nbl);
    if (NblStatus == NDIS_STATUS_SUCCESS) {
      AEROVNET_NBL_SET_STATUS(Nbl, TxStatus);
    }
  }

  Pending = AEROVNET_NBL_GET_PENDING(Nbl);
  Pending--;
  AEROVNET_NBL_SET_PENDING(Nbl, Pending);

  if (Pending == 0) {
    FinalStatus = AEROVNET_NBL_GET_STATUS(Nbl);
    AEROVNET_NBL_SET_PENDING(Nbl, 0);
    AEROVNET_NBL_SET_STATUS(Nbl, NDIS_STATUS_SUCCESS);

    NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;
    if (*CompleteNblTail) {
      NET_BUFFER_LIST_NEXT_NBL(*CompleteNblTail) = Nbl;
      *CompleteNblTail = Nbl;
    } else {
      *CompleteNblHead = Nbl;
      *CompleteNblTail = Nbl;
    }

    NET_BUFFER_LIST_STATUS(Nbl) = FinalStatus;
  }
}

static VOID AerovNetCompleteTxRequest(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_TX_REQUEST* TxReq, _In_ NDIS_STATUS TxStatus,
                                     _Inout_ PNET_BUFFER_LIST* CompleteNblHead, _Inout_ PNET_BUFFER_LIST* CompleteNblTail) {
  if (!TxReq || !TxReq->Nbl) {
    return;
  }

  AerovNetTxNblCompleteOneNetBufferLocked(Adapter, TxReq->Nbl, TxStatus, CompleteNblHead, CompleteNblTail);
}

static BOOLEAN AerovNetIsBroadcastAddress(_In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* Mac) {
  ULONG I;
  for (I = 0; I < ETH_LENGTH_OF_ADDRESS; I++) {
    if (Mac[I] != 0xFF) {
      return FALSE;
    }
  }
  return TRUE;
}

static BOOLEAN AerovNetMacEqual(_In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* A, _In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* B) {
  return (RtlCompareMemory(A, B, ETH_LENGTH_OF_ADDRESS) == ETH_LENGTH_OF_ADDRESS) ? TRUE : FALSE;
}

static BOOLEAN AerovNetAcceptFrame(_In_ const AEROVNET_ADAPTER* Adapter, _In_reads_bytes_(FrameLen) const UCHAR* Frame, _In_ ULONG FrameLen) {
  const UCHAR* Dst;
  ULONG Filter;

  if (FrameLen < 14) {
    return FALSE;
  }

  Filter = Adapter->PacketFilter;
  if (Filter == 0) {
    return FALSE;
  }

  if (Filter & NDIS_PACKET_TYPE_PROMISCUOUS) {
    return TRUE;
  }

  Dst = Frame;

  if (AerovNetIsBroadcastAddress(Dst)) {
    return (Filter & NDIS_PACKET_TYPE_BROADCAST) ? TRUE : FALSE;
  }

  if (Dst[0] & 0x01) {
    if (Filter & NDIS_PACKET_TYPE_ALL_MULTICAST) {
      return TRUE;
    }

    if (Filter & NDIS_PACKET_TYPE_MULTICAST) {
      ULONG I;
      for (I = 0; I < Adapter->MulticastListSize; I++) {
        if (AerovNetMacEqual(Dst, Adapter->MulticastList[I])) {
          return TRUE;
        }
      }
    }

    return FALSE;
  }

  // Unicast.
  if ((Filter & NDIS_PACKET_TYPE_DIRECTED) == 0) {
    return FALSE;
  }

  return AerovNetMacEqual(Dst, Adapter->CurrentMac) ? TRUE : FALSE;
}

static VOID AerovNetGenerateFallbackMac(_Out_writes_(ETH_LENGTH_OF_ADDRESS) UCHAR* Mac) {
  LARGE_INTEGER T;

  KeQuerySystemTime(&T);

  // Locally administered, unicast.
  Mac[0] = 0x02;
  Mac[1] = (UCHAR)(T.LowPart & 0xFF);
  Mac[2] = (UCHAR)((T.LowPart >> 8) & 0xFF);
  Mac[3] = (UCHAR)((T.LowPart >> 16) & 0xFF);
  Mac[4] = (UCHAR)((T.LowPart >> 24) & 0xFF);
  Mac[5] = (UCHAR)(T.HighPart & 0xFF);
}

static NDIS_STATUS AerovNetParseResources(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ PNDIS_RESOURCE_LIST Resources) {
  ULONG I;
  NDIS_STATUS Status;

  Adapter->Bar0Va = NULL;
  Adapter->Bar0Length = 0;
  Adapter->Bar0Pa.QuadPart = 0;

  Adapter->CommonCfg = NULL;
  Adapter->NotifyBase = NULL;
  Adapter->NotifyOffMultiplier = 0;
  Adapter->IsrCfg = NULL;
  Adapter->DeviceCfg = NULL;

  if (!Resources) {
    return NDIS_STATUS_RESOURCES;
  }

  for (I = 0; I < Resources->Count; I++) {
    PCM_PARTIAL_RESOURCE_DESCRIPTOR Desc = &Resources->PartialDescriptors[I];
    if (Desc->Type == CmResourceTypeMemory && Desc->u.Memory.Length >= AEROVNET_BAR0_MIN_LEN) {
      Adapter->Bar0Pa = Desc->u.Memory.Start;
      Adapter->Bar0Length = Desc->u.Memory.Length;
      break;
    }
  }

  if (Adapter->Bar0Length < AEROVNET_BAR0_MIN_LEN) {
    return NDIS_STATUS_RESOURCES;
  }

  {
    NDIS_PHYSICAL_ADDRESS Pa;
    Pa.QuadPart = Adapter->Bar0Pa.QuadPart;

    Status = NdisMMapIoSpace((PVOID*)&Adapter->Bar0Va, Adapter->MiniportAdapterHandle, Pa, Adapter->Bar0Length);
  }
  if (Status != NDIS_STATUS_SUCCESS) {
    Adapter->Bar0Va = NULL;
    Adapter->Bar0Length = 0;
    Adapter->Bar0Pa.QuadPart = 0;
    return Status;
  }

  Adapter->CommonCfg = (volatile virtio_pci_common_cfg*)(Adapter->Bar0Va + AEROVNET_MMIO_COMMON_CFG_OFF);
  Adapter->NotifyBase = (volatile UCHAR*)(Adapter->Bar0Va + AEROVNET_MMIO_NOTIFY_OFF);
  Adapter->NotifyOffMultiplier = AEROVNET_NOTIFY_OFF_MULTIPLIER;
  Adapter->IsrCfg = (volatile UCHAR*)(Adapter->Bar0Va + AEROVNET_MMIO_ISR_OFF);
  Adapter->DeviceCfg = (volatile UCHAR*)(Adapter->Bar0Va + AEROVNET_MMIO_DEVICE_CFG_OFF);

  return Status;
}

static VOID AerovNetFreeRxBuffer(_Inout_ AEROVNET_RX_BUFFER* Rx) {
  if (Rx->Nbl) {
    NdisFreeNetBufferList(Rx->Nbl);
    Rx->Nbl = NULL;
    Rx->Nb = NULL;
  }

  if (Rx->Mdl) {
    IoFreeMdl(Rx->Mdl);
    Rx->Mdl = NULL;
  }

  if (Rx->BufferVa) {
    MmFreeContiguousMemory(Rx->BufferVa);
    Rx->BufferVa = NULL;
  }
}

static VOID AerovNetFreeTxResources(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG I;

  if (Adapter->TxRequests) {
    for (I = 0; I < Adapter->TxRequestCount; I++) {
      // SG lists are owned by NDIS; if any request is still holding one, we
      // cannot safely free it here without the corresponding NET_BUFFER.
      Adapter->TxRequests[I].SgList = NULL;
    }

    ExFreePoolWithTag(Adapter->TxRequests, AEROVNET_TAG);
    Adapter->TxRequests = NULL;
  }

  Adapter->TxRequestCount = 0;
  InitializeListHead(&Adapter->TxFreeList);
  InitializeListHead(&Adapter->TxAwaitingSgList);
  InitializeListHead(&Adapter->TxPendingList);
  InitializeListHead(&Adapter->TxSubmittedList);

  if (Adapter->TxHeaderBlockVa) {
    MmFreeContiguousMemory(Adapter->TxHeaderBlockVa);
    Adapter->TxHeaderBlockVa = NULL;
    Adapter->TxHeaderBlockBytes = 0;
    Adapter->TxHeaderBlockPa.QuadPart = 0;
  }
}

static VOID AerovNetFreeRxResources(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG I;

  if (Adapter->RxBuffers) {
    for (I = 0; I < Adapter->RxBufferCount; I++) {
      AerovNetFreeRxBuffer(&Adapter->RxBuffers[I]);
    }

    ExFreePoolWithTag(Adapter->RxBuffers, AEROVNET_TAG);
    Adapter->RxBuffers = NULL;
  }

  Adapter->RxBufferCount = 0;
  InitializeListHead(&Adapter->RxFreeList);
}

static VOID AerovNetFreeVq(_Inout_ AEROVNET_VQ* Vq) {
  if (!Vq) {
    return;
  }

  if (Vq->RingVa) {
    MmFreeContiguousMemory(Vq->RingVa);
    Vq->RingVa = NULL;
  }
  Vq->RingPa = 0;
  Vq->RingBytes = 0;

  if (Vq->IndirectVa) {
    MmFreeContiguousMemory(Vq->IndirectVa);
    Vq->IndirectVa = NULL;
  }
  Vq->IndirectPa = 0;
  Vq->IndirectBytes = 0;

  if (Vq->Vq) {
    ExFreePoolWithTag(Vq->Vq, AEROVNET_TAG);
    Vq->Vq = NULL;
  }

  Vq->QueueIndex = 0;
  Vq->QueueSize = 0;
  Vq->NotifyAddr = NULL;
}

static VOID AerovNetCleanupAdapter(_Inout_ AEROVNET_ADAPTER* Adapter) {
  if (!Adapter) {
    return;
  }

  // Device is already stopped/reset by the caller.
  AerovNetFreeTxResources(Adapter);
  AerovNetFreeRxResources(Adapter);

  if (Adapter->NblPool) {
    NdisFreeNetBufferListPool(Adapter->NblPool);
    Adapter->NblPool = NULL;
  }

  if (Adapter->DmaHandle) {
    NdisMDeregisterScatterGatherDma(Adapter->DmaHandle);
    Adapter->DmaHandle = NULL;
  }

  if (Adapter->InterruptHandle) {
    NdisMDeregisterInterruptEx(Adapter->InterruptHandle);
    Adapter->InterruptHandle = NULL;
  }

  AerovNetFreeVq(&Adapter->RxVq);
  AerovNetFreeVq(&Adapter->TxVq);

  if (Adapter->Bar0Va) {
    NdisMUnmapIoSpace(Adapter->MiniportAdapterHandle, Adapter->Bar0Va, Adapter->Bar0Length);
    Adapter->Bar0Va = NULL;
    Adapter->Bar0Length = 0;
    Adapter->Bar0Pa.QuadPart = 0;
  }

  Adapter->CommonCfg = NULL;
  Adapter->NotifyBase = NULL;
  Adapter->NotifyOffMultiplier = 0;
  Adapter->IsrCfg = NULL;
  Adapter->DeviceCfg = NULL;

  NdisFreeSpinLock(&Adapter->Lock);

  ExFreePoolWithTag(Adapter, AEROVNET_TAG);
}

static VOID AerovNetFillRxQueueLocked(_Inout_ AEROVNET_ADAPTER* Adapter) {
  BOOLEAN Notify = FALSE;

  while (!IsListEmpty(&Adapter->RxFreeList)) {
    PLIST_ENTRY Entry;
    AEROVNET_RX_BUFFER* Rx;
    VIRTQ_SG Sg[2];
    UINT16 Head;
    NTSTATUS Status;

    // Each receive buffer is posted as a 2-descriptor chain: header + payload.
    if (!Adapter->RxVq.Vq || Adapter->RxVq.Vq->num_free < 2) {
      break;
    }

    Entry = RemoveHeadList(&Adapter->RxFreeList);
    Rx = CONTAINING_RECORD(Entry, AEROVNET_RX_BUFFER, Link);

    Rx->Indicated = FALSE;

    Sg[0].addr = (UINT64)Rx->BufferPa.QuadPart;
    Sg[0].len = (UINT32)sizeof(VIRTIO_NET_HDR);
    Sg[0].write = TRUE;

    Sg[1].addr = (UINT64)Rx->BufferPa.QuadPart + (UINT64)sizeof(VIRTIO_NET_HDR);
    Sg[1].len = (UINT32)(Rx->BufferBytes - sizeof(VIRTIO_NET_HDR));
    Sg[1].write = TRUE;

    Status = VirtqSplitAddBuffer(Adapter->RxVq.Vq, Sg, 2, Rx, &Head);
    if (!NT_SUCCESS(Status)) {
      InsertHeadList(&Adapter->RxFreeList, &Rx->Link);
      break;
    }

    VirtqSplitPublish(Adapter->RxVq.Vq, Head);
    Notify = TRUE;
  }

  if (Notify) {
    if (VirtqSplitKickPrepare(Adapter->RxVq.Vq)) {
      WRITE_REGISTER_USHORT((volatile USHORT*)Adapter->RxVq.NotifyAddr, Adapter->RxVq.QueueIndex);
      VirtqSplitKickCommit(Adapter->RxVq.Vq);
    }
  }
}

static VOID AerovNetFlushTxPendingLocked(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PLIST_ENTRY CompleteTxReqs,
                                        _Inout_ PNET_BUFFER_LIST* CompleteNblHead, _Inout_ PNET_BUFFER_LIST* CompleteNblTail) {
  VIRTQ_SG Sg[AEROVNET_MAX_TX_SG_ELEMENTS + 1];
  BOOLEAN Notified = FALSE;

  while (!IsListEmpty(&Adapter->TxPendingList)) {
    AEROVNET_TX_REQUEST* TxReq;
    UINT16 Needed;
    ULONG I;
    NTSTATUS Status;

    TxReq = CONTAINING_RECORD(Adapter->TxPendingList.Flink, AEROVNET_TX_REQUEST, Link);
    if (TxReq->Cancelled) {
      RemoveEntryList(&TxReq->Link);
      InsertTailList(CompleteTxReqs, &TxReq->Link);
      AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_REQUEST_ABORTED, CompleteNblHead, CompleteNblTail);
      continue;
    }

    if (!TxReq->SgList || TxReq->SgList->NumberOfElements > AEROVNET_MAX_TX_SG_ELEMENTS) {
      RemoveEntryList(&TxReq->Link);
      InsertTailList(CompleteTxReqs, &TxReq->Link);
      AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_BUFFER_OVERFLOW, CompleteNblHead, CompleteNblTail);
      continue;
    }

    RtlZeroMemory(TxReq->HeaderVa, sizeof(VIRTIO_NET_HDR));

    Needed = (UINT16)(TxReq->SgList->NumberOfElements + 1);

    Sg[0].addr = (UINT64)TxReq->HeaderPa.QuadPart;
    Sg[0].len = (UINT32)sizeof(VIRTIO_NET_HDR);
    Sg[0].write = FALSE;

    for (I = 0; I < TxReq->SgList->NumberOfElements; I++) {
      Sg[1 + I].addr = (UINT64)TxReq->SgList->Elements[I].Address.QuadPart;
      Sg[1 + I].len = (UINT32)TxReq->SgList->Elements[I].Length;
      Sg[1 + I].write = FALSE;
    }

    Status = VirtqSplitAddBuffer(Adapter->TxVq.Vq, Sg, Needed, TxReq, &TxReq->DescHeadId);
    if (!NT_SUCCESS(Status)) {
      break;
    }

    RemoveEntryList(&TxReq->Link);
    VirtqSplitPublish(Adapter->TxVq.Vq, TxReq->DescHeadId);

    TxReq->State = AerovNetTxSubmitted;
    InsertTailList(&Adapter->TxSubmittedList, &TxReq->Link);
    Notified = TRUE;
  }

  if (Notified) {
    if (VirtqSplitKickPrepare(Adapter->TxVq.Vq)) {
      WRITE_REGISTER_USHORT((volatile USHORT*)Adapter->TxVq.NotifyAddr, Adapter->TxVq.QueueIndex);
      VirtqSplitKickCommit(Adapter->TxVq.Vq);
    }
  }
}

static NDIS_STATUS AerovNetAllocateRxResources(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG I;
  PHYSICAL_ADDRESS Low = {0};
  PHYSICAL_ADDRESS High;
  PHYSICAL_ADDRESS Skip = {0};

  High.QuadPart = ~0ull;

  InitializeListHead(&Adapter->RxFreeList);
  Adapter->RxBufferCount = Adapter->RxVq.QueueSize;

  Adapter->RxBuffers = (AEROVNET_RX_BUFFER*)ExAllocatePoolWithTag(NonPagedPool, sizeof(AEROVNET_RX_BUFFER) * Adapter->RxBufferCount, AEROVNET_TAG);
  if (!Adapter->RxBuffers) {
    return NDIS_STATUS_RESOURCES;
  }
  RtlZeroMemory(Adapter->RxBuffers, sizeof(AEROVNET_RX_BUFFER) * Adapter->RxBufferCount);

  for (I = 0; I < Adapter->RxBufferCount; I++) {
    AEROVNET_RX_BUFFER* Rx = &Adapter->RxBuffers[I];

    Rx->BufferBytes = Adapter->RxBufferTotalBytes;
    Rx->BufferVa = MmAllocateContiguousMemorySpecifyCache(Rx->BufferBytes, Low, High, Skip, MmCached);
    if (!Rx->BufferVa) {
      return NDIS_STATUS_RESOURCES;
    }

    Rx->BufferPa = MmGetPhysicalAddress(Rx->BufferVa);

    Rx->Mdl = IoAllocateMdl(Rx->BufferVa, Rx->BufferBytes, FALSE, FALSE, NULL);
    if (!Rx->Mdl) {
      return NDIS_STATUS_RESOURCES;
    }
    MmBuildMdlForNonPagedPool(Rx->Mdl);

    Rx->Nbl = NdisAllocateNetBufferAndNetBufferList(Adapter->NblPool, 0, 0, Rx->Mdl, sizeof(VIRTIO_NET_HDR), 0);
    if (!Rx->Nbl) {
      return NDIS_STATUS_RESOURCES;
    }

    Rx->Nb = NET_BUFFER_LIST_FIRST_NB(Rx->Nbl);
    Rx->Indicated = FALSE;

    Rx->Nbl->MiniportReserved[0] = Rx;

    InsertTailList(&Adapter->RxFreeList, &Rx->Link);
  }

  return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetAllocateTxResources(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG I;
  PHYSICAL_ADDRESS Low = {0};
  PHYSICAL_ADDRESS High;
  PHYSICAL_ADDRESS Skip = {0};

  High.QuadPart = ~0ull;

  InitializeListHead(&Adapter->TxFreeList);
  InitializeListHead(&Adapter->TxAwaitingSgList);
  InitializeListHead(&Adapter->TxPendingList);
  InitializeListHead(&Adapter->TxSubmittedList);

  Adapter->TxRequestCount = Adapter->TxVq.QueueSize;
  Adapter->TxRequests =
      (AEROVNET_TX_REQUEST*)ExAllocatePoolWithTag(NonPagedPool, sizeof(AEROVNET_TX_REQUEST) * Adapter->TxRequestCount, AEROVNET_TAG);
  if (!Adapter->TxRequests) {
    return NDIS_STATUS_RESOURCES;
  }
  RtlZeroMemory(Adapter->TxRequests, sizeof(AEROVNET_TX_REQUEST) * Adapter->TxRequestCount);

  Adapter->TxHeaderBlockBytes = sizeof(VIRTIO_NET_HDR) * Adapter->TxRequestCount;
  Adapter->TxHeaderBlockVa = MmAllocateContiguousMemorySpecifyCache(Adapter->TxHeaderBlockBytes, Low, High, Skip, MmCached);
  if (!Adapter->TxHeaderBlockVa) {
    return NDIS_STATUS_RESOURCES;
  }
  Adapter->TxHeaderBlockPa = MmGetPhysicalAddress(Adapter->TxHeaderBlockVa);
  RtlZeroMemory(Adapter->TxHeaderBlockVa, Adapter->TxHeaderBlockBytes);

  for (I = 0; I < Adapter->TxRequestCount; I++) {
    AEROVNET_TX_REQUEST* Tx = &Adapter->TxRequests[I];
    RtlZeroMemory(Tx, sizeof(*Tx));

    Tx->State = AerovNetTxFree;
    Tx->Cancelled = FALSE;
    Tx->Adapter = Adapter;
    Tx->HeaderVa = Adapter->TxHeaderBlockVa + (sizeof(VIRTIO_NET_HDR) * I);
    Tx->HeaderPa.QuadPart = Adapter->TxHeaderBlockPa.QuadPart + (sizeof(VIRTIO_NET_HDR) * I);
    InsertTailList(&Adapter->TxFreeList, &Tx->Link);
  }

  return NDIS_STATUS_SUCCESS;
}

static __forceinline UCHAR AerovNetVirtioReadDeviceStatus(_In_ const AEROVNET_ADAPTER* Adapter) {
  return READ_REGISTER_UCHAR((volatile UCHAR*)&Adapter->CommonCfg->device_status);
}

static __forceinline VOID AerovNetVirtioWriteDeviceStatus(_In_ const AEROVNET_ADAPTER* Adapter, _In_ UCHAR Status) {
  WRITE_REGISTER_UCHAR((volatile UCHAR*)&Adapter->CommonCfg->device_status, Status);
}

static VOID AerovNetVirtioResetDevice(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG WaitedUs;
  const ULONG ResetTimeoutUs = 1000000u;
  const ULONG ResetPollDelayUs = 1000u;

  if (!Adapter || !Adapter->CommonCfg) {
    return;
  }

  KeMemoryBarrier();
  AerovNetVirtioWriteDeviceStatus(Adapter, 0);
  KeMemoryBarrier();

  // Avoid long polling if called above PASSIVE_LEVEL (e.g. surprise removal paths).
  if (KeGetCurrentIrql() > PASSIVE_LEVEL) {
    return;
  }

  for (WaitedUs = 0; WaitedUs < ResetTimeoutUs; WaitedUs += ResetPollDelayUs) {
    if (AerovNetVirtioReadDeviceStatus(Adapter) == 0) {
      KeMemoryBarrier();
      return;
    }
    KeStallExecutionProcessor(ResetPollDelayUs);
  }
}

static __forceinline VOID AerovNetVirtioAddStatus(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ UCHAR Bits) {
  UCHAR Status;

  if (!Adapter || !Adapter->CommonCfg) {
    return;
  }

  KeMemoryBarrier();
  Status = AerovNetVirtioReadDeviceStatus(Adapter);
  Status |= Bits;
  AerovNetVirtioWriteDeviceStatus(Adapter, Status);
  KeMemoryBarrier();
}

static __forceinline UCHAR AerovNetVirtioGetStatus(_Inout_ AEROVNET_ADAPTER* Adapter) {
  if (!Adapter || !Adapter->CommonCfg) {
    return 0;
  }

  KeMemoryBarrier();
  return AerovNetVirtioReadDeviceStatus(Adapter);
}

static UINT64 AerovNetVirtioReadHostFeatures(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG Lo;
  ULONG Hi;

  if (!Adapter || !Adapter->CommonCfg) {
    return 0;
  }

  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->device_feature_select, 0);
  KeMemoryBarrier();
  Lo = READ_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->device_feature);
  KeMemoryBarrier();

  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->device_feature_select, 1);
  KeMemoryBarrier();
  Hi = READ_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->device_feature);
  KeMemoryBarrier();

  return ((UINT64)Hi << 32) | (UINT64)Lo;
}

static VOID AerovNetVirtioWriteGuestFeatures(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ UINT64 Features) {
  ULONG Lo;
  ULONG Hi;

  if (!Adapter || !Adapter->CommonCfg) {
    return;
  }

  Lo = (ULONG)(Features & 0xFFFFFFFFui64);
  Hi = (ULONG)(Features >> 32);

  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->driver_feature_select, 0);
  KeMemoryBarrier();
  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->driver_feature, Lo);
  KeMemoryBarrier();

  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->driver_feature_select, 1);
  KeMemoryBarrier();
  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->driver_feature, Hi);
  KeMemoryBarrier();
}

static __forceinline VOID AerovNetVirtioSelectQueue(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ USHORT QueueIndex) {
  WRITE_REGISTER_USHORT((volatile USHORT*)&Adapter->CommonCfg->queue_select, QueueIndex);
  KeMemoryBarrier();
}

static NDIS_STATUS AerovNetSetupVq(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_VQ* Vq, _In_ USHORT QueueIndex,
                                  _In_ USHORT ExpectedQueueSize, _In_ USHORT IndirectMaxDesc) {
  USHORT QueueSize;
  USHORT NotifyOff;
  size_t VqBytes;
  size_t RingBytes;
  PHYSICAL_ADDRESS Low = {0};
  PHYSICAL_ADDRESS High;
  PHYSICAL_ADDRESS Skip = {0};
  NTSTATUS NtStatus;

  High.QuadPart = ~0ull;

  if (!Adapter || !Adapter->CommonCfg || !Adapter->NotifyBase || !Vq) {
    return NDIS_STATUS_FAILURE;
  }

  RtlZeroMemory(Vq, sizeof(*Vq));
  Vq->QueueIndex = QueueIndex;

  AerovNetVirtioSelectQueue(Adapter, QueueIndex);
  QueueSize = READ_REGISTER_USHORT((volatile USHORT*)&Adapter->CommonCfg->queue_size);
  NotifyOff = READ_REGISTER_USHORT((volatile USHORT*)&Adapter->CommonCfg->queue_notify_off);

  if (QueueSize != ExpectedQueueSize) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  // Contract v1: notify_off_multiplier=4 and queue_notify_off(q)=q.
  if (NotifyOff != QueueIndex) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Vq->QueueSize = QueueSize;

  VqBytes = VirtqSplitStateSize(QueueSize);
  Vq->Vq = (VIRTQ_SPLIT*)ExAllocatePoolWithTag(NonPagedPool, VqBytes, AEROVNET_TAG);
  if (!Vq->Vq) {
    return NDIS_STATUS_RESOURCES;
  }

  RingBytes = VirtqSplitRingMemSize(QueueSize, 4, FALSE);
  if (RingBytes == 0 || RingBytes > 0xFFFFFFFFu) {
    return NDIS_STATUS_RESOURCES;
  }

  Vq->RingBytes = (ULONG)RingBytes;
  Vq->RingVa = MmAllocateContiguousMemorySpecifyCache(Vq->RingBytes, Low, High, Skip, MmCached);
  if (!Vq->RingVa) {
    return NDIS_STATUS_RESOURCES;
  }
  RtlZeroMemory(Vq->RingVa, Vq->RingBytes);
  Vq->RingPa = (UINT64)MmGetPhysicalAddress(Vq->RingVa).QuadPart;

  if (IndirectMaxDesc != 0) {
    const UINT16 TableCount = QueueSize;
    size_t IndirectBytes = sizeof(VIRTQ_DESC) * (size_t)TableCount * (size_t)IndirectMaxDesc;

    if (IndirectBytes == 0 || IndirectBytes > 0xFFFFFFFFu) {
      return NDIS_STATUS_RESOURCES;
    }

    Vq->IndirectBytes = (ULONG)IndirectBytes;
    Vq->IndirectVa = MmAllocateContiguousMemorySpecifyCache(Vq->IndirectBytes, Low, High, Skip, MmCached);
    if (!Vq->IndirectVa) {
      return NDIS_STATUS_RESOURCES;
    }
    RtlZeroMemory(Vq->IndirectVa, Vq->IndirectBytes);
    Vq->IndirectPa = (UINT64)MmGetPhysicalAddress(Vq->IndirectVa).QuadPart;

    NtStatus = VirtqSplitInit(Vq->Vq,
                             QueueSize,
                             FALSE,
                             TRUE,
                             Vq->RingVa,
                             Vq->RingPa,
                             4,
                             Vq->IndirectVa,
                             Vq->IndirectPa,
                             TableCount,
                             IndirectMaxDesc);
  } else {
    NtStatus = VirtqSplitInit(Vq->Vq, QueueSize, FALSE, TRUE, Vq->RingVa, Vq->RingPa, 4, NULL, 0, 0, 0);
  }

  if (!NT_SUCCESS(NtStatus)) {
    return NDIS_STATUS_FAILURE;
  }

  Vq->NotifyAddr = (volatile USHORT*)((volatile UCHAR*)Adapter->NotifyBase + ((ULONG)NotifyOff * Adapter->NotifyOffMultiplier));

  // Disable MSI-X for this queue; INTx/ISR is required by contract v1.
  AerovNetVirtioSelectQueue(Adapter, QueueIndex);
  WRITE_REGISTER_USHORT((volatile USHORT*)&Adapter->CommonCfg->queue_msix_vector, 0xFFFFu);

  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->queue_desc_lo, (ULONG)(Vq->Vq->desc_pa & 0xFFFFFFFFui64));
  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->queue_desc_hi, (ULONG)(Vq->Vq->desc_pa >> 32));

  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->queue_avail_lo, (ULONG)(Vq->Vq->avail_pa & 0xFFFFFFFFui64));
  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->queue_avail_hi, (ULONG)(Vq->Vq->avail_pa >> 32));

  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->queue_used_lo, (ULONG)(Vq->Vq->used_pa & 0xFFFFFFFFui64));
  WRITE_REGISTER_ULONG((volatile ULONG*)&Adapter->CommonCfg->queue_used_hi, (ULONG)(Vq->Vq->used_pa >> 32));

  WRITE_REGISTER_USHORT((volatile USHORT*)&Adapter->CommonCfg->queue_enable, 1);
  KeMemoryBarrier();

  return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetVirtioStart(_Inout_ AEROVNET_ADAPTER* Adapter) {
  NDIS_STATUS Status;
  UCHAR DevStatus;
  UCHAR Mac[ETH_LENGTH_OF_ADDRESS];
  USHORT LinkStatus;
  UINT64 RequiredFeatures;
  UINT64 WantedFeatures;
  UCHAR RevisionId;
  ULONG BytesRead;

  if (!Adapter || !Adapter->CommonCfg || !Adapter->DeviceCfg || !Adapter->IsrCfg || !Adapter->NotifyBase) {
    return NDIS_STATUS_FAILURE;
  }

  // Contract major version is encoded in PCI Revision ID.
  RevisionId = 0;
  BytesRead = NdisReadPciSlotInformation(Adapter->MiniportAdapterHandle, 0, &RevisionId, 0x08, sizeof(RevisionId));
  if (BytesRead != sizeof(RevisionId) || RevisionId != AEROVNET_PCI_REVISION_ID) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  AerovNetVirtioResetDevice(Adapter);
  AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_ACKNOWLEDGE);
  AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_DRIVER);

  Adapter->HostFeatures = AerovNetVirtioReadHostFeatures(Adapter);

  RequiredFeatures = VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS;
  WantedFeatures = RequiredFeatures;

  if ((Adapter->HostFeatures & RequiredFeatures) != RequiredFeatures) {
    AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_FAILED);
    AerovNetVirtioResetDevice(Adapter);
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Adapter->GuestFeatures = Adapter->HostFeatures & WantedFeatures;
  AerovNetVirtioWriteGuestFeatures(Adapter, Adapter->GuestFeatures);

  AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_FEATURES_OK);
  DevStatus = AerovNetVirtioGetStatus(Adapter);
  if ((DevStatus & VIRTIO_STATUS_FEATURES_OK) == 0) {
    AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_FAILED);
    AerovNetVirtioResetDevice(Adapter);
    return NDIS_STATUS_FAILURE;
  }

  // Disable MSI-X config interrupt vector; INTx/ISR is required by contract v1.
  WRITE_REGISTER_USHORT((volatile USHORT*)&Adapter->CommonCfg->msix_config, 0xFFFFu);

  // Read virtio-net device config (MAC + link status).
  RtlZeroMemory(Mac, sizeof(Mac));
  {
    ULONG i;
    for (i = 0; i < ETH_LENGTH_OF_ADDRESS; i++) {
      Mac[i] = READ_REGISTER_UCHAR((volatile UCHAR*)&Adapter->DeviceCfg[i]);
    }
  }
  RtlCopyMemory(Adapter->PermanentMac, Mac, ETH_LENGTH_OF_ADDRESS);
  RtlCopyMemory(Adapter->CurrentMac, Mac, ETH_LENGTH_OF_ADDRESS);

  LinkStatus = READ_REGISTER_USHORT((volatile USHORT*)&Adapter->DeviceCfg[ETH_LENGTH_OF_ADDRESS]);
  Adapter->LinkUp = (LinkStatus & VIRTIO_NET_S_LINK_UP) ? TRUE : FALSE;

  // Virtqueues: 0 = RX, 1 = TX.
  if (READ_REGISTER_USHORT((volatile USHORT*)&Adapter->CommonCfg->num_queues) < 2) {
    AerovNetVirtioResetDevice(Adapter);
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Status = AerovNetSetupVq(Adapter, &Adapter->RxVq, 0, 256, 0);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetVirtioResetDevice(Adapter);
    return Status;
  }

  Status = AerovNetSetupVq(Adapter, &Adapter->TxVq, 1, 256, (USHORT)(AEROVNET_MAX_TX_SG_ELEMENTS + 1));
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetVirtioResetDevice(Adapter);
    return Status;
  }

  // Allocate packet buffers.
  Adapter->Mtu = AEROVNET_MTU_DEFAULT;
  Adapter->MaxFrameSize = Adapter->Mtu + 14;

  Adapter->RxBufferDataBytes = 2048;
  Adapter->RxBufferTotalBytes = sizeof(VIRTIO_NET_HDR) + Adapter->RxBufferDataBytes;

  Status = AerovNetAllocateRxResources(Adapter);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetVirtioResetDevice(Adapter);
    return Status;
  }

  Status = AerovNetAllocateTxResources(Adapter);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetVirtioResetDevice(Adapter);
    return Status;
  }

  // Pre-post RX buffers.
  NdisAcquireSpinLock(&Adapter->Lock);
  AerovNetFillRxQueueLocked(Adapter);
  NdisReleaseSpinLock(&Adapter->Lock);

  AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_DRIVER_OK);
  return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetVirtioStop(_Inout_ AEROVNET_ADAPTER* Adapter) {
  LIST_ENTRY AbortTxReqs;
  PNET_BUFFER_LIST CompleteHead;
  PNET_BUFFER_LIST CompleteTail;

  if (!Adapter) {
    return;
  }

  // Stop the device first to prevent further DMA/interrupts.
  AerovNetVirtioResetDevice(Adapter);

  // HaltEx is expected to run at PASSIVE_LEVEL; waiting here avoids freeing
  // memory while an NDIS SG mapping callback might still reference it.
  if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
    (VOID)KeWaitForSingleObject(&Adapter->OutstandingSgEvent, Executive, KernelMode, FALSE, NULL);
  }

  InitializeListHead(&AbortTxReqs);
  CompleteHead = NULL;
  CompleteTail = NULL;

  // Move all outstanding TX requests to a local list and complete their NBLs.
  NdisAcquireSpinLock(&Adapter->Lock);

  while (!IsListEmpty(&Adapter->TxAwaitingSgList)) {
    PLIST_ENTRY E = RemoveHeadList(&Adapter->TxAwaitingSgList);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(E, AEROVNET_TX_REQUEST, Link);
    InsertTailList(&AbortTxReqs, &TxReq->Link);
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_RESET_IN_PROGRESS, &CompleteHead, &CompleteTail);
  }

  while (!IsListEmpty(&Adapter->TxPendingList)) {
    PLIST_ENTRY E = RemoveHeadList(&Adapter->TxPendingList);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(E, AEROVNET_TX_REQUEST, Link);
    InsertTailList(&AbortTxReqs, &TxReq->Link);
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_RESET_IN_PROGRESS, &CompleteHead, &CompleteTail);
  }

  while (!IsListEmpty(&Adapter->TxSubmittedList)) {
    PLIST_ENTRY E = RemoveHeadList(&Adapter->TxSubmittedList);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(E, AEROVNET_TX_REQUEST, Link);
    InsertTailList(&AbortTxReqs, &TxReq->Link);
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_RESET_IN_PROGRESS, &CompleteHead, &CompleteTail);
  }

  NdisReleaseSpinLock(&Adapter->Lock);

  // Free per-request SG lists and return requests to the free list.
  while (!IsListEmpty(&AbortTxReqs)) {
    PLIST_ENTRY E = RemoveHeadList(&AbortTxReqs);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(E, AEROVNET_TX_REQUEST, Link);
    PNET_BUFFER Nb = TxReq->Nb;

    if (TxReq->SgList) {
      NdisMFreeNetBufferSGList(Adapter->DmaHandle, TxReq->SgList, Nb);
      TxReq->SgList = NULL;
    }

    NdisAcquireSpinLock(&Adapter->Lock);
    AerovNetFreeTxRequestNoLock(Adapter, TxReq);
    NdisReleaseSpinLock(&Adapter->Lock);
  }

  while (CompleteHead) {
    PNET_BUFFER_LIST Nbl = CompleteHead;
    CompleteHead = NET_BUFFER_LIST_NEXT_NBL(Nbl);
    NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;
    AerovNetCompleteNblSend(Adapter, Nbl, NET_BUFFER_LIST_STATUS(Nbl));
  }

  AerovNetFreeTxResources(Adapter);
  AerovNetFreeRxResources(Adapter);

  AerovNetFreeVq(&Adapter->RxVq);
  AerovNetFreeVq(&Adapter->TxVq);
}

static VOID AerovNetIndicateLinkState(_In_ AEROVNET_ADAPTER* Adapter) {
  NDIS_STATUS_INDICATION Ind;
  NDIS_LINK_STATE LinkState;

  RtlZeroMemory(&Ind, sizeof(Ind));
  RtlZeroMemory(&LinkState, sizeof(LinkState));

  LinkState.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
  LinkState.Header.Revision = NDIS_LINK_STATE_REVISION_1;
  LinkState.Header.Size = sizeof(LinkState);

  LinkState.MediaConnectState = Adapter->LinkUp ? MediaConnectStateConnected : MediaConnectStateDisconnected;
  LinkState.MediaDuplexState = MediaDuplexStateFull;
  LinkState.XmitLinkSpeed = g_DefaultLinkSpeedBps;
  LinkState.RcvLinkSpeed = g_DefaultLinkSpeedBps;

  Ind.Header.Type = NDIS_OBJECT_TYPE_STATUS_INDICATION;
  Ind.Header.Revision = NDIS_STATUS_INDICATION_REVISION_1;
  Ind.Header.Size = sizeof(Ind);

  Ind.SourceHandle = Adapter->MiniportAdapterHandle;
  Ind.StatusCode = NDIS_STATUS_LINK_STATE;
  Ind.StatusBuffer = &LinkState;
  Ind.StatusBufferSize = sizeof(LinkState);

  NdisMIndicateStatusEx(Adapter->MiniportAdapterHandle, &Ind);
}

static BOOLEAN AerovNetInterruptIsr(_In_ NDIS_HANDLE MiniportInterruptContext, _Out_ PBOOLEAN QueueDefaultInterruptDpc,
                                   _Out_ PULONG TargetProcessors) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportInterruptContext;
  UCHAR Isr;

  UNREFERENCED_PARAMETER(TargetProcessors);

  if (!Adapter) {
    return FALSE;
  }

  if (Adapter->State == AerovNetAdapterStopped) {
    return FALSE;
  }

  if (!Adapter->IsrCfg) {
    return FALSE;
  }

  // Contract v1: ISR is a read-to-ack 8-bit register at BAR0 + 0x2000.
  Isr = READ_REGISTER_UCHAR(Adapter->IsrCfg);
  if (Isr == 0) {
    return FALSE;
  }

  InterlockedOr(&Adapter->IsrStatus, (LONG)Isr);

  *QueueDefaultInterruptDpc = TRUE;
  return TRUE;
}

static VOID AerovNetInterruptDpc(_In_ NDIS_HANDLE MiniportInterruptContext, _In_ PVOID MiniportDpcContext,
                                _In_ PULONG NdisReserved1, _In_ PULONG NdisReserved2) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportInterruptContext;
  LONG Isr;
  LIST_ENTRY CompleteTxReqs;
  PNET_BUFFER_LIST CompleteNblHead;
  PNET_BUFFER_LIST CompleteNblTail;
  PNET_BUFFER_LIST IndicateHead;
  PNET_BUFFER_LIST IndicateTail;
  ULONG IndicateCount;
  BOOLEAN LinkChanged;
  BOOLEAN NewLinkUp;

  UNREFERENCED_PARAMETER(MiniportDpcContext);
  UNREFERENCED_PARAMETER(NdisReserved1);
  UNREFERENCED_PARAMETER(NdisReserved2);

  if (!Adapter) {
    return;
  }

  InitializeListHead(&CompleteTxReqs);
  CompleteNblHead = NULL;
  CompleteNblTail = NULL;
  IndicateHead = NULL;
  IndicateTail = NULL;
  IndicateCount = 0;
  LinkChanged = FALSE;
  NewLinkUp = Adapter->LinkUp;

  Isr = InterlockedExchange(&Adapter->IsrStatus, 0);

  NdisAcquireSpinLock(&Adapter->Lock);

  if (Adapter->State == AerovNetAdapterStopped) {
    NdisReleaseSpinLock(&Adapter->Lock);
    return;
  }

  // TX completions.
  for (;;) {
    PVOID Cookie;
    AEROVNET_TX_REQUEST* TxReq;
    NTSTATUS VqStatus;

    Cookie = NULL;

    if (!Adapter->TxVq.Vq) {
      break;
    }

    VqStatus = VirtqSplitGetUsed(Adapter->TxVq.Vq, &Cookie, NULL);
    if (VqStatus == STATUS_NOT_FOUND) {
      break;
    }
    if (!NT_SUCCESS(VqStatus)) {
      Adapter->StatTxErrors++;
      break;
    }

    TxReq = (AEROVNET_TX_REQUEST*)Cookie;

    if (TxReq) {
      Adapter->StatTxPackets++;
      Adapter->StatTxBytes += NET_BUFFER_DATA_LENGTH(TxReq->Nb);

      if (TxReq->State == AerovNetTxSubmitted) {
        RemoveEntryList(&TxReq->Link);
      }
      InsertTailList(&CompleteTxReqs, &TxReq->Link);

      AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_SUCCESS, &CompleteNblHead, &CompleteNblTail);
    }
  }

  // Submit any TX requests that were waiting on descriptors.
  if (Adapter->State == AerovNetAdapterRunning) {
    AerovNetFlushTxPendingLocked(Adapter, &CompleteTxReqs, &CompleteNblHead, &CompleteNblTail);
  }

  // RX completions.
  for (;;) {
    PVOID Cookie;
    UINT32 UsedLen;
    AEROVNET_RX_BUFFER* Rx;
    ULONG PayloadLen;
    NTSTATUS VqStatus;

    Cookie = NULL;
    UsedLen = 0;

    if (!Adapter->RxVq.Vq) {
      break;
    }

    VqStatus = VirtqSplitGetUsed(Adapter->RxVq.Vq, &Cookie, &UsedLen);
    if (VqStatus == STATUS_NOT_FOUND) {
      break;
    }
    if (!NT_SUCCESS(VqStatus)) {
      Adapter->StatRxErrors++;
      break;
    }

    Rx = (AEROVNET_RX_BUFFER*)Cookie;

    if (!Rx) {
      continue;
    }

    if (UsedLen < sizeof(VIRTIO_NET_HDR) || UsedLen > Rx->BufferBytes) {
      Adapter->StatRxErrors++;
      InsertTailList(&Adapter->RxFreeList, &Rx->Link);
      continue;
    }

    PayloadLen = UsedLen - sizeof(VIRTIO_NET_HDR);

    // Contract v1: drop undersized/oversized Ethernet frames but always recycle.
    if (PayloadLen < 14 || PayloadLen > 1514) {
      Adapter->StatRxErrors++;
      InsertTailList(&Adapter->RxFreeList, &Rx->Link);
      continue;
    }

    if (Adapter->State != AerovNetAdapterRunning) {
      InsertTailList(&Adapter->RxFreeList, &Rx->Link);
      continue;
    }

    if (!AerovNetAcceptFrame(Adapter, Rx->BufferVa + sizeof(VIRTIO_NET_HDR), PayloadLen)) {
      InsertTailList(&Adapter->RxFreeList, &Rx->Link);
      continue;
    }

    Rx->Indicated = TRUE;

    NET_BUFFER_DATA_OFFSET(Rx->Nb) = sizeof(VIRTIO_NET_HDR);
    NET_BUFFER_DATA_LENGTH(Rx->Nb) = PayloadLen;
    NET_BUFFER_LIST_STATUS(Rx->Nbl) = NDIS_STATUS_SUCCESS;
    NET_BUFFER_LIST_NEXT_NBL(Rx->Nbl) = NULL;

    if (IndicateTail) {
      NET_BUFFER_LIST_NEXT_NBL(IndicateTail) = Rx->Nbl;
      IndicateTail = Rx->Nbl;
    } else {
      IndicateHead = Rx->Nbl;
      IndicateTail = Rx->Nbl;
    }

    IndicateCount++;
    Adapter->StatRxPackets++;
    Adapter->StatRxBytes += PayloadLen;
  }

  // Refill RX queue with any buffers we dropped.
  if (Adapter->State == AerovNetAdapterRunning) {
    AerovNetFillRxQueueLocked(Adapter);
  }

  // Link state change handling (config interrupt). Keep it cheap: read status only if supported.
  if ((Isr & 0x2) != 0 && (Adapter->GuestFeatures & VIRTIO_NET_F_STATUS) != 0 && Adapter->DeviceCfg) {
    USHORT LinkStatus;

    LinkStatus = READ_REGISTER_USHORT((volatile USHORT*)&Adapter->DeviceCfg[ETH_LENGTH_OF_ADDRESS]);
    NewLinkUp = (LinkStatus & VIRTIO_NET_S_LINK_UP) ? TRUE : FALSE;
    if (NewLinkUp != Adapter->LinkUp) {
      Adapter->LinkUp = NewLinkUp;
      LinkChanged = TRUE;
    }
  }

  NdisReleaseSpinLock(&Adapter->Lock);

  // Free SG lists and return TX requests to free list.
  while (!IsListEmpty(&CompleteTxReqs)) {
    PLIST_ENTRY Entry = RemoveHeadList(&CompleteTxReqs);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);

    if (TxReq->SgList) {
      NdisMFreeNetBufferSGList(Adapter->DmaHandle, TxReq->SgList, TxReq->Nb);
      TxReq->SgList = NULL;
    }

    NdisAcquireSpinLock(&Adapter->Lock);
    AerovNetFreeTxRequestNoLock(Adapter, TxReq);
    NdisReleaseSpinLock(&Adapter->Lock);
  }

  // Complete any NBLs which have no remaining NET_BUFFERs pending.
  while (CompleteNblHead) {
    PNET_BUFFER_LIST Nbl = CompleteNblHead;
    CompleteNblHead = NET_BUFFER_LIST_NEXT_NBL(Nbl);
    NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;

    AerovNetCompleteNblSend(Adapter, Nbl, NET_BUFFER_LIST_STATUS(Nbl));
  }

  // Indicate receives.
  if (IndicateHead) {
    NdisMIndicateReceiveNetBufferLists(Adapter->MiniportAdapterHandle, IndicateHead, NDIS_DEFAULT_PORT_NUMBER, IndicateCount,
                                       AerovNetReceiveIndicationFlagsForCurrentIrql());
  }

  if (LinkChanged) {
    AerovNetIndicateLinkState(Adapter);
  }
}

static VOID AerovNetProcessSgList(_In_ PDEVICE_OBJECT DeviceObject, _In_opt_ PVOID Reserved,
                                 _In_ PSCATTER_GATHER_LIST ScatterGatherList, _In_ PVOID Context) {
  AEROVNET_TX_REQUEST* TxReq;
  AEROVNET_ADAPTER* Adapter;
  VIRTQ_SG Sg[AEROVNET_MAX_TX_SG_ELEMENTS + 1];
  ULONG ElemCount;
  UINT16 Needed;
  ULONG I;
  NTSTATUS Status;
  PNET_BUFFER NbForFree;
  BOOLEAN CompleteNow;
  PNET_BUFFER_LIST CompleteHead;
  PNET_BUFFER_LIST CompleteTail;

  UNREFERENCED_PARAMETER(DeviceObject);
  UNREFERENCED_PARAMETER(Reserved);

  TxReq = (AEROVNET_TX_REQUEST*)Context;
  if (!TxReq || !ScatterGatherList) {
    return;
  }

  Adapter = TxReq->Adapter;
  if (!Adapter) {
    return;
  }

  ElemCount = ScatterGatherList->NumberOfElements;
  Needed = (USHORT)(ElemCount + 1);

  CompleteNow = FALSE;
  CompleteHead = NULL;
  CompleteTail = NULL;
  NbForFree = TxReq->Nb;

  NdisAcquireSpinLock(&Adapter->Lock);

  // The request was in-flight in the "awaiting SG" list. Remove it regardless
  // of whether it will be submitted or completed with an error.
  if (TxReq->State == AerovNetTxAwaitingSg) {
    RemoveEntryList(&TxReq->Link);
  }

  TxReq->SgList = ScatterGatherList;

  if (TxReq->Cancelled) {
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_REQUEST_ABORTED, &CompleteHead, &CompleteTail);
    CompleteNow = TRUE;
  } else if (Adapter->State == AerovNetAdapterStopped) {
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_RESET_IN_PROGRESS, &CompleteHead, &CompleteTail);
    CompleteNow = TRUE;
  } else if (ElemCount > AEROVNET_MAX_TX_SG_ELEMENTS) {
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_BUFFER_OVERFLOW, &CompleteHead, &CompleteTail);
    CompleteNow = TRUE;
  } else if (Adapter->State != AerovNetAdapterRunning) {
    // Paused: queue for later retry on restart.
    TxReq->State = AerovNetTxPendingSubmit;
    InsertTailList(&Adapter->TxPendingList, &TxReq->Link);
  } else {
    // Prepare virtio descriptors: header + payload SG elements.
    RtlZeroMemory(TxReq->HeaderVa, sizeof(VIRTIO_NET_HDR));

    Sg[0].addr = (UINT64)TxReq->HeaderPa.QuadPart;
    Sg[0].len = (UINT32)sizeof(VIRTIO_NET_HDR);
    Sg[0].write = FALSE;

    for (I = 0; I < ElemCount; I++) {
      Sg[1 + I].addr = (UINT64)ScatterGatherList->Elements[I].Address.QuadPart;
      Sg[1 + I].len = (UINT32)ScatterGatherList->Elements[I].Length;
      Sg[1 + I].write = FALSE;
    }

    Status = VirtqSplitAddBuffer(Adapter->TxVq.Vq, Sg, Needed, TxReq, &TxReq->DescHeadId);
    if (!NT_SUCCESS(Status)) {
      // No descriptors yet; queue it for later retry (DPC will flush).
      TxReq->State = AerovNetTxPendingSubmit;
      InsertTailList(&Adapter->TxPendingList, &TxReq->Link);
    } else {
      VirtqSplitPublish(Adapter->TxVq.Vq, TxReq->DescHeadId);
      TxReq->State = AerovNetTxSubmitted;
      InsertTailList(&Adapter->TxSubmittedList, &TxReq->Link);
      if (VirtqSplitKickPrepare(Adapter->TxVq.Vq)) {
        WRITE_REGISTER_USHORT((volatile USHORT*)Adapter->TxVq.NotifyAddr, Adapter->TxVq.QueueIndex);
        VirtqSplitKickCommit(Adapter->TxVq.Vq);
      }
    }
  }

  NdisReleaseSpinLock(&Adapter->Lock);

  if (CompleteNow) {
    // Free the SG list immediately; the device never saw the descriptors.
    if (ScatterGatherList) {
      NdisMFreeNetBufferSGList(Adapter->DmaHandle, ScatterGatherList, NbForFree);
    }

    NdisAcquireSpinLock(&Adapter->Lock);
    AerovNetFreeTxRequestNoLock(Adapter, TxReq);
    NdisReleaseSpinLock(&Adapter->Lock);

    while (CompleteHead) {
      PNET_BUFFER_LIST Nbl = CompleteHead;
      CompleteHead = NET_BUFFER_LIST_NEXT_NBL(Nbl);
      NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;
      AerovNetCompleteNblSend(Adapter, Nbl, NET_BUFFER_LIST_STATUS(Nbl));
    }
  }

  // Signal HaltEx once all SG mapping callbacks have finished.
  if (InterlockedDecrement(&Adapter->OutstandingSgMappings) == 0) {
    KeSetEvent(&Adapter->OutstandingSgEvent, IO_NO_INCREMENT, FALSE);
  }
}

static NDIS_STATUS AerovNetOidQuery(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PNDIS_OID_REQUEST OidRequest) {
  NDIS_OID Oid = OidRequest->DATA.QUERY_INFORMATION.Oid;
  PVOID OutBuffer = OidRequest->DATA.QUERY_INFORMATION.InformationBuffer;
  ULONG OutLen = OidRequest->DATA.QUERY_INFORMATION.InformationBufferLength;
  ULONG BytesWritten = 0;
  ULONG BytesNeeded = 0;

  switch (Oid) {
    case OID_GEN_SUPPORTED_LIST: {
      BytesNeeded = sizeof(g_SupportedOids);
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, g_SupportedOids, sizeof(g_SupportedOids));
      BytesWritten = sizeof(g_SupportedOids);
      break;
    }

    case OID_GEN_HARDWARE_STATUS: {
      NDIS_HARDWARE_STATUS Hw = NdisHardwareStatusReady;
      BytesNeeded = sizeof(Hw);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(NDIS_HARDWARE_STATUS*)OutBuffer = Hw;
      BytesWritten = sizeof(Hw);
      break;
    }

    case OID_GEN_MEDIA_SUPPORTED:
    case OID_GEN_MEDIA_IN_USE: {
      NDIS_MEDIUM M = NdisMedium802_3;
      BytesNeeded = sizeof(M);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(NDIS_MEDIUM*)OutBuffer = M;
      BytesWritten = sizeof(M);
      break;
    }

    case OID_GEN_PHYSICAL_MEDIUM: {
      NDIS_PHYSICAL_MEDIUM P = NdisPhysicalMedium802_3;
      BytesNeeded = sizeof(P);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(NDIS_PHYSICAL_MEDIUM*)OutBuffer = P;
      BytesWritten = sizeof(P);
      break;
    }

    case OID_GEN_MAXIMUM_FRAME_SIZE: {
      ULONG V = Adapter->Mtu;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_MAXIMUM_LOOKAHEAD:
    case OID_GEN_CURRENT_LOOKAHEAD: {
      ULONG V = Adapter->Mtu;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_MAXIMUM_TOTAL_SIZE: {
      ULONG V = Adapter->MaxFrameSize;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_LINK_SPEED: {
      ULONG Speed100Bps = (ULONG)(g_DefaultLinkSpeedBps / 100ull);
      BytesNeeded = sizeof(Speed100Bps);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = Speed100Bps;
      BytesWritten = sizeof(Speed100Bps);
      break;
    }

    case OID_GEN_TRANSMIT_BLOCK_SIZE:
    case OID_GEN_RECEIVE_BLOCK_SIZE: {
      ULONG V = 1;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_VENDOR_ID: {
      ULONG Vid = ((ULONG)Adapter->PermanentMac[0]) | ((ULONG)Adapter->PermanentMac[1] << 8) | ((ULONG)Adapter->PermanentMac[2] << 16);
      BytesNeeded = sizeof(Vid);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = Vid;
      BytesWritten = sizeof(Vid);
      break;
    }

    case OID_GEN_VENDOR_DESCRIPTION: {
      static const char Desc[] = "Aero virtio-net";
      BytesNeeded = sizeof(Desc);
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, Desc, sizeof(Desc));
      BytesWritten = sizeof(Desc);
      break;
    }

    case OID_GEN_DRIVER_VERSION: {
      USHORT V = AEROVNET_OID_DRIVER_VERSION;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(USHORT*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_VENDOR_DRIVER_VERSION: {
      ULONG V = 0x00010000; // 1.0
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_MAC_OPTIONS: {
      ULONG V = NDIS_MAC_OPTION_COPY_LOOKAHEAD_DATA | NDIS_MAC_OPTION_NO_LOOPBACK;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_MEDIA_CONNECT_STATUS: {
      NDIS_MEDIA_STATE S = Adapter->LinkUp ? NdisMediaStateConnected : NdisMediaStateDisconnected;
      BytesNeeded = sizeof(S);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(NDIS_MEDIA_STATE*)OutBuffer = S;
      BytesWritten = sizeof(S);
      break;
    }

    case OID_GEN_CURRENT_PACKET_FILTER: {
      ULONG V = Adapter->PacketFilter;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_MAXIMUM_SEND_PACKETS: {
      ULONG V = 1;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_802_3_PERMANENT_ADDRESS: {
      BytesNeeded = ETH_LENGTH_OF_ADDRESS;
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, Adapter->PermanentMac, ETH_LENGTH_OF_ADDRESS);
      BytesWritten = ETH_LENGTH_OF_ADDRESS;
      break;
    }

    case OID_802_3_CURRENT_ADDRESS: {
      BytesNeeded = ETH_LENGTH_OF_ADDRESS;
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, Adapter->CurrentMac, ETH_LENGTH_OF_ADDRESS);
      BytesWritten = ETH_LENGTH_OF_ADDRESS;
      break;
    }

    case OID_802_3_MULTICAST_LIST: {
      BytesNeeded = Adapter->MulticastListSize * ETH_LENGTH_OF_ADDRESS;
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, Adapter->MulticastList, BytesNeeded);
      BytesWritten = BytesNeeded;
      break;
    }

    case OID_802_3_MAXIMUM_LIST_SIZE: {
      ULONG V = NDIS_MAX_MULTICAST_LIST;
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_LINK_STATE: {
      NDIS_LINK_STATE LS;
      RtlZeroMemory(&LS, sizeof(LS));
      LS.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
      LS.Header.Revision = NDIS_LINK_STATE_REVISION_1;
      LS.Header.Size = sizeof(LS);
      LS.MediaConnectState = Adapter->LinkUp ? MediaConnectStateConnected : MediaConnectStateDisconnected;
      LS.MediaDuplexState = MediaDuplexStateFull;
      LS.XmitLinkSpeed = g_DefaultLinkSpeedBps;
      LS.RcvLinkSpeed = g_DefaultLinkSpeedBps;

      BytesNeeded = sizeof(LS);
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, &LS, sizeof(LS));
      BytesWritten = sizeof(LS);
      break;
    }

    case OID_GEN_XMIT_OK: {
      ULONG V = (ULONG)min(Adapter->StatTxPackets, (ULONGLONG)0xFFFFFFFF);
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_RCV_OK: {
      ULONG V = (ULONG)min(Adapter->StatRxPackets, (ULONGLONG)0xFFFFFFFF);
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_XMIT_ERROR: {
      ULONG V = (ULONG)min(Adapter->StatTxErrors, (ULONGLONG)0xFFFFFFFF);
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_RCV_ERROR: {
      ULONG V = (ULONG)min(Adapter->StatRxErrors, (ULONGLONG)0xFFFFFFFF);
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_RCV_NO_BUFFER: {
      ULONG V = (ULONG)min(Adapter->StatRxNoBuffers, (ULONGLONG)0xFFFFFFFF);
      BytesNeeded = sizeof(V);
      if (OutLen < BytesNeeded) {
        break;
      }
      *(ULONG*)OutBuffer = V;
      BytesWritten = sizeof(V);
      break;
    }

    case OID_GEN_STATISTICS: {
      NDIS_STATISTICS_INFO Info;
      RtlZeroMemory(&Info, sizeof(Info));
      Info.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
      Info.Header.Revision = NDIS_STATISTICS_INFO_REVISION_1;
      Info.Header.Size = sizeof(Info);
      Info.SupportedStatistics = NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_RCV |
                                NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_XMIT |
                                NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_RCV |
                                NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_XMIT;
      Info.ifInUcastPkts = Adapter->StatRxPackets;
      Info.ifOutUcastPkts = Adapter->StatTxPackets;
      Info.ifInUcastOctets = Adapter->StatRxBytes;
      Info.ifOutUcastOctets = Adapter->StatTxBytes;

      BytesNeeded = sizeof(Info);
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, &Info, sizeof(Info));
      BytesWritten = sizeof(Info);
      break;
    }

    default:
      return NDIS_STATUS_NOT_SUPPORTED;
  }

  if (BytesWritten == 0 && BytesNeeded != 0 && OutLen < BytesNeeded) {
    OidRequest->DATA.QUERY_INFORMATION.BytesNeeded = BytesNeeded;
    return NDIS_STATUS_BUFFER_TOO_SHORT;
  }

  OidRequest->DATA.QUERY_INFORMATION.BytesWritten = BytesWritten;
  return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetOidSet(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PNDIS_OID_REQUEST OidRequest) {
  NDIS_OID Oid = OidRequest->DATA.SET_INFORMATION.Oid;
  PVOID InBuffer = OidRequest->DATA.SET_INFORMATION.InformationBuffer;
  ULONG InLen = OidRequest->DATA.SET_INFORMATION.InformationBufferLength;
  ULONG BytesRead = 0;
  ULONG BytesNeeded = 0;

  switch (Oid) {
    case OID_GEN_CURRENT_PACKET_FILTER: {
      ULONG Filter;
      BytesNeeded = sizeof(Filter);
      if (InLen < BytesNeeded) {
        break;
      }
      Filter = *(ULONG*)InBuffer;

      // We support only standard Ethernet filters.
      if (Filter & ~(NDIS_PACKET_TYPE_DIRECTED | NDIS_PACKET_TYPE_MULTICAST | NDIS_PACKET_TYPE_ALL_MULTICAST |
                     NDIS_PACKET_TYPE_BROADCAST | NDIS_PACKET_TYPE_PROMISCUOUS)) {
        return NDIS_STATUS_NOT_SUPPORTED;
      }

      Adapter->PacketFilter = Filter;
      BytesRead = sizeof(Filter);
      break;
    }

    case OID_GEN_CURRENT_LOOKAHEAD: {
      ULONG V;
      BytesNeeded = sizeof(V);
      if (InLen < BytesNeeded) {
        break;
      }

      V = *(ULONG*)InBuffer;
      if (V > Adapter->Mtu) {
        return NDIS_STATUS_INVALID_DATA;
      }

      // We always indicate full frames; treat lookahead as advisory.
      BytesRead = sizeof(V);
      break;
    }

    case OID_802_3_MULTICAST_LIST: {
      ULONG Count;
      if ((InLen % ETH_LENGTH_OF_ADDRESS) != 0) {
        return NDIS_STATUS_INVALID_LENGTH;
      }

      Count = InLen / ETH_LENGTH_OF_ADDRESS;
      if (Count > NDIS_MAX_MULTICAST_LIST) {
        return NDIS_STATUS_MULTICAST_FULL;
      }

      Adapter->MulticastListSize = Count;
      if (Count) {
        RtlCopyMemory(Adapter->MulticastList, InBuffer, InLen);
      }

      BytesRead = InLen;
      break;
    }

    default:
      return NDIS_STATUS_NOT_SUPPORTED;
  }

  if (BytesRead == 0 && BytesNeeded != 0 && InLen < BytesNeeded) {
    OidRequest->DATA.SET_INFORMATION.BytesNeeded = BytesNeeded;
    return NDIS_STATUS_BUFFER_TOO_SHORT;
  }

  OidRequest->DATA.SET_INFORMATION.BytesRead = BytesRead;
  return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetMiniportOidRequest(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNDIS_OID_REQUEST OidRequest) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
  NDIS_STATUS Status;

  if (!Adapter) {
    return NDIS_STATUS_FAILURE;
  }

  NdisAcquireSpinLock(&Adapter->Lock);
  if (Adapter->State == AerovNetAdapterStopped) {
    NdisReleaseSpinLock(&Adapter->Lock);
    return NDIS_STATUS_RESET_IN_PROGRESS;
  }
  NdisReleaseSpinLock(&Adapter->Lock);

  switch (OidRequest->RequestType) {
    case NdisRequestQueryInformation:
    case NdisRequestQueryStatistics:
      Status = AerovNetOidQuery(Adapter, OidRequest);
      break;
    case NdisRequestSetInformation:
      Status = AerovNetOidSet(Adapter, OidRequest);
      break;
    default:
      Status = NDIS_STATUS_NOT_SUPPORTED;
      break;
  }

  return Status;
}

static VOID AerovNetMiniportSendNetBufferLists(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNET_BUFFER_LIST NetBufferLists,
                                               _In_ NDIS_PORT_NUMBER PortNumber, _In_ ULONG SendFlags) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
  PNET_BUFFER_LIST Nbl;
  PNET_BUFFER_LIST CompleteHead;
  PNET_BUFFER_LIST CompleteTail;

  UNREFERENCED_PARAMETER(PortNumber);
  UNREFERENCED_PARAMETER(SendFlags);

  if (!Adapter) {
    return;
  }

  CompleteHead = NULL;
  CompleteTail = NULL;

  Nbl = NetBufferLists;
  while (Nbl) {
    PNET_BUFFER_LIST NextNbl;
    PNET_BUFFER Nb;
    LONG NbCount;

    NextNbl = NET_BUFFER_LIST_NEXT_NBL(Nbl);
    NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;

    NbCount = 0;
    for (Nb = NET_BUFFER_LIST_FIRST_NB(Nbl); Nb; Nb = NET_BUFFER_NEXT_NB(Nb)) {
      NbCount++;
    }

    if (NbCount == 0) {
      NET_BUFFER_LIST_STATUS(Nbl) = NDIS_STATUS_SUCCESS;
      if (CompleteTail) {
        NET_BUFFER_LIST_NEXT_NBL(CompleteTail) = Nbl;
        CompleteTail = Nbl;
      } else {
        CompleteHead = Nbl;
        CompleteTail = Nbl;
      }

      Nbl = NextNbl;
      continue;
    }

    AEROVNET_NBL_SET_PENDING(Nbl, NbCount);
    AEROVNET_NBL_SET_STATUS(Nbl, NDIS_STATUS_SUCCESS);

    for (Nb = NET_BUFFER_LIST_FIRST_NB(Nbl); Nb; Nb = NET_BUFFER_NEXT_NB(Nb)) {
      AEROVNET_TX_REQUEST* TxReq;
      NDIS_STATUS SgStatus;

      TxReq = NULL;

      NdisAcquireSpinLock(&Adapter->Lock);

      if (Adapter->State != AerovNetAdapterRunning) {
        AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, NDIS_STATUS_RESET_IN_PROGRESS, &CompleteHead, &CompleteTail);
        NdisReleaseSpinLock(&Adapter->Lock);
        continue;
      }

      if (IsListEmpty(&Adapter->TxFreeList)) {
        AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, NDIS_STATUS_RESOURCES, &CompleteHead, &CompleteTail);
        NdisReleaseSpinLock(&Adapter->Lock);
        continue;
      }

      {
        PLIST_ENTRY Entry = RemoveHeadList(&Adapter->TxFreeList);
        TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);
      }

      TxReq->State = AerovNetTxAwaitingSg;
      TxReq->Cancelled = FALSE;
      TxReq->Adapter = Adapter;
      TxReq->Nbl = Nbl;
      TxReq->Nb = Nb;
      TxReq->SgList = NULL;
      InsertTailList(&Adapter->TxAwaitingSgList, &TxReq->Link);

      if (InterlockedIncrement(&Adapter->OutstandingSgMappings) == 1) {
        KeClearEvent(&Adapter->OutstandingSgEvent);
      }

      NdisReleaseSpinLock(&Adapter->Lock);

      SgStatus = NdisMAllocateNetBufferSGList(Adapter->DmaHandle, Nb, TxReq, 0);
      if (SgStatus != NDIS_STATUS_SUCCESS && SgStatus != NDIS_STATUS_PENDING) {
        // SG allocation failed synchronously; undo the TxReq.
        if (InterlockedDecrement(&Adapter->OutstandingSgMappings) == 0) {
          KeSetEvent(&Adapter->OutstandingSgEvent, IO_NO_INCREMENT, FALSE);
        }

        NdisAcquireSpinLock(&Adapter->Lock);
        RemoveEntryList(&TxReq->Link);
        AerovNetCompleteTxRequest(Adapter, TxReq, SgStatus, &CompleteHead, &CompleteTail);
        AerovNetFreeTxRequestNoLock(Adapter, TxReq);
        NdisReleaseSpinLock(&Adapter->Lock);
      }
    }

    Nbl = NextNbl;
  }

  while (CompleteHead) {
    PNET_BUFFER_LIST Done = CompleteHead;
    CompleteHead = NET_BUFFER_LIST_NEXT_NBL(Done);
    NET_BUFFER_LIST_NEXT_NBL(Done) = NULL;
    AerovNetCompleteNblSend(Adapter, Done, NET_BUFFER_LIST_STATUS(Done));
  }
}

static VOID AerovNetMiniportReturnNetBufferLists(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNET_BUFFER_LIST NetBufferLists,
                                                 _In_ ULONG ReturnFlags) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
  PNET_BUFFER_LIST Nbl;

  UNREFERENCED_PARAMETER(ReturnFlags);

  if (!Adapter) {
    return;
  }

  NdisAcquireSpinLock(&Adapter->Lock);

  for (Nbl = NetBufferLists; Nbl; Nbl = NET_BUFFER_LIST_NEXT_NBL(Nbl)) {
    AEROVNET_RX_BUFFER* Rx = (AEROVNET_RX_BUFFER*)Nbl->MiniportReserved[0];
    if (!Rx) {
      continue;
    }

    Rx->Indicated = FALSE;
    NET_BUFFER_DATA_OFFSET(Rx->Nb) = sizeof(VIRTIO_NET_HDR);
    NET_BUFFER_DATA_LENGTH(Rx->Nb) = 0;

    InsertTailList(&Adapter->RxFreeList, &Rx->Link);
  }

  if (Adapter->State == AerovNetAdapterRunning) {
    AerovNetFillRxQueueLocked(Adapter);
  }

  NdisReleaseSpinLock(&Adapter->Lock);
}

static VOID AerovNetMiniportCancelSend(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PVOID CancelId) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
  PLIST_ENTRY Entry;
  LIST_ENTRY CancelledReqs;
  PNET_BUFFER_LIST CompleteHead;
  PNET_BUFFER_LIST CompleteTail;

  if (!Adapter) {
    return;
  }

  InitializeListHead(&CancelledReqs);
  CompleteHead = NULL;
  CompleteTail = NULL;

  NdisAcquireSpinLock(&Adapter->Lock);

  // Mark any requests still awaiting SG mapping as cancelled; they will be
  // completed in the SG callback once the mapping finishes.
  for (Entry = Adapter->TxAwaitingSgList.Flink; Entry != &Adapter->TxAwaitingSgList; Entry = Entry->Flink) {
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);
    if (NET_BUFFER_LIST_CANCEL_ID(TxReq->Nbl) == CancelId) {
      TxReq->Cancelled = TRUE;
    }
  }

  // Cancel requests queued pending submission (SG mapping already complete).
  Entry = Adapter->TxPendingList.Flink;
  while (Entry != &Adapter->TxPendingList) {
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);
    Entry = Entry->Flink;

    if (NET_BUFFER_LIST_CANCEL_ID(TxReq->Nbl) == CancelId) {
      RemoveEntryList(&TxReq->Link);
      InsertTailList(&CancelledReqs, &TxReq->Link);
      AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_REQUEST_ABORTED, &CompleteHead, &CompleteTail);
    }
  }

  NdisReleaseSpinLock(&Adapter->Lock);

  while (!IsListEmpty(&CancelledReqs)) {
    PLIST_ENTRY E = RemoveHeadList(&CancelledReqs);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(E, AEROVNET_TX_REQUEST, Link);
    PNET_BUFFER Nb = TxReq->Nb;

    if (TxReq->SgList) {
      NdisMFreeNetBufferSGList(Adapter->DmaHandle, TxReq->SgList, Nb);
      TxReq->SgList = NULL;
    }

    NdisAcquireSpinLock(&Adapter->Lock);
    AerovNetFreeTxRequestNoLock(Adapter, TxReq);
    NdisReleaseSpinLock(&Adapter->Lock);
  }

  while (CompleteHead) {
    PNET_BUFFER_LIST Nbl = CompleteHead;
    CompleteHead = NET_BUFFER_LIST_NEXT_NBL(Nbl);
    NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;
    AerovNetCompleteNblSend(Adapter, Nbl, NET_BUFFER_LIST_STATUS(Nbl));
  }
}

static VOID AerovNetMiniportDevicePnPEventNotify(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNET_DEVICE_PNP_EVENT NetDevicePnPEvent) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;

  if (!Adapter || !NetDevicePnPEvent) {
    return;
  }

  if (NetDevicePnPEvent->DevicePnPEvent == NdisDevicePnPEventSurpriseRemoved) {
    NdisAcquireSpinLock(&Adapter->Lock);
    Adapter->SurpriseRemoved = TRUE;
    Adapter->State = AerovNetAdapterStopped;
    NdisReleaseSpinLock(&Adapter->Lock);

    // Quiesce the device. Full cleanup happens in HaltEx (PASSIVE_LEVEL).
    AerovNetVirtioResetDevice(Adapter);
  }
}

static NDIS_STATUS AerovNetMiniportPause(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNDIS_MINIPORT_PAUSE_PARAMETERS PauseParameters) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;

  UNREFERENCED_PARAMETER(PauseParameters);

  if (!Adapter) {
    return NDIS_STATUS_FAILURE;
  }

  NdisAcquireSpinLock(&Adapter->Lock);
  Adapter->State = AerovNetAdapterPaused;
  NdisReleaseSpinLock(&Adapter->Lock);

  return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetMiniportRestart(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNDIS_MINIPORT_RESTART_PARAMETERS RestartParameters) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
  LIST_ENTRY CompleteTxReqs;
  PNET_BUFFER_LIST CompleteHead;
  PNET_BUFFER_LIST CompleteTail;

  UNREFERENCED_PARAMETER(RestartParameters);

  if (!Adapter) {
    return NDIS_STATUS_FAILURE;
  }

  InitializeListHead(&CompleteTxReqs);
  CompleteHead = NULL;
  CompleteTail = NULL;

  NdisAcquireSpinLock(&Adapter->Lock);
  Adapter->State = AerovNetAdapterRunning;
  AerovNetFillRxQueueLocked(Adapter);
  AerovNetFlushTxPendingLocked(Adapter, &CompleteTxReqs, &CompleteHead, &CompleteTail);
  NdisReleaseSpinLock(&Adapter->Lock);

  while (!IsListEmpty(&CompleteTxReqs)) {
    PLIST_ENTRY E = RemoveHeadList(&CompleteTxReqs);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(E, AEROVNET_TX_REQUEST, Link);
    PNET_BUFFER Nb = TxReq->Nb;

    if (TxReq->SgList) {
      NdisMFreeNetBufferSGList(Adapter->DmaHandle, TxReq->SgList, Nb);
      TxReq->SgList = NULL;
    }

    NdisAcquireSpinLock(&Adapter->Lock);
    AerovNetFreeTxRequestNoLock(Adapter, TxReq);
    NdisReleaseSpinLock(&Adapter->Lock);
  }

  while (CompleteHead) {
    PNET_BUFFER_LIST Nbl = CompleteHead;
    CompleteHead = NET_BUFFER_LIST_NEXT_NBL(Nbl);
    NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;
    AerovNetCompleteNblSend(Adapter, Nbl, NET_BUFFER_LIST_STATUS(Nbl));
  }

  return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetMiniportHaltEx(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ NDIS_HALT_ACTION HaltAction) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;

  UNREFERENCED_PARAMETER(HaltAction);

  if (!Adapter) {
    return;
  }

  NdisAcquireSpinLock(&Adapter->Lock);
  Adapter->State = AerovNetAdapterStopped;
  NdisReleaseSpinLock(&Adapter->Lock);

  AerovNetVirtioStop(Adapter);
  AerovNetCleanupAdapter(Adapter);
}

static NDIS_STATUS AerovNetMiniportInitializeEx(_In_ NDIS_HANDLE MiniportAdapterHandle, _In_ NDIS_HANDLE MiniportDriverContext,
                                                _In_ PNDIS_MINIPORT_INIT_PARAMETERS MiniportInitParameters) {
  NDIS_STATUS Status;
  AEROVNET_ADAPTER* Adapter;
  NDIS_MINIPORT_ADAPTER_REGISTRATION_ATTRIBUTES Reg;
  NDIS_MINIPORT_ADAPTER_GENERAL_ATTRIBUTES Gen;
  NDIS_MINIPORT_INTERRUPT_CHARACTERISTICS Intr;
  NDIS_SG_DMA_DESCRIPTION DmaDesc;
  NDIS_NET_BUFFER_LIST_POOL_PARAMETERS PoolParams;

  UNREFERENCED_PARAMETER(MiniportDriverContext);

  Adapter = (AEROVNET_ADAPTER*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*Adapter), AEROVNET_TAG);
  if (!Adapter) {
    return NDIS_STATUS_RESOURCES;
  }
  RtlZeroMemory(Adapter, sizeof(*Adapter));

  Adapter->MiniportAdapterHandle = MiniportAdapterHandle;
  Adapter->State = AerovNetAdapterStopped;
  Adapter->PacketFilter = NDIS_PACKET_TYPE_DIRECTED | NDIS_PACKET_TYPE_BROADCAST | NDIS_PACKET_TYPE_MULTICAST;
  Adapter->MulticastListSize = 0;
  Adapter->IsrStatus = 0;
  Adapter->OutstandingSgMappings = 0;

  NdisAllocateSpinLock(&Adapter->Lock);
  KeInitializeEvent(&Adapter->OutstandingSgEvent, NotificationEvent, TRUE);

  InitializeListHead(&Adapter->RxFreeList);
  InitializeListHead(&Adapter->TxFreeList);
  InitializeListHead(&Adapter->TxAwaitingSgList);
  InitializeListHead(&Adapter->TxPendingList);
  InitializeListHead(&Adapter->TxSubmittedList);

  // Registration attributes.
  RtlZeroMemory(&Reg, sizeof(Reg));
  Reg.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_ADAPTER_REGISTRATION_ATTRIBUTES;
  Reg.Header.Revision = NDIS_MINIPORT_ADAPTER_REGISTRATION_ATTRIBUTES_REVISION_1;
  Reg.Header.Size = sizeof(Reg);
  Reg.MiniportAdapterContext = Adapter;
  Reg.AttributeFlags = NDIS_MINIPORT_ATTRIBUTES_HARDWARE_DEVICE | NDIS_MINIPORT_ATTRIBUTES_BUS_MASTER;
  Reg.CheckForHangTimeInSeconds = 0;
  Reg.InterfaceType = NdisInterfacePci;

  Status = NdisMSetMiniportAttributes(MiniportAdapterHandle, (PNDIS_MINIPORT_ADAPTER_ATTRIBUTES)&Reg);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetCleanupAdapter(Adapter);
    return Status;
  }

  Status = AerovNetParseResources(Adapter, MiniportInitParameters->AllocatedResources);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetCleanupAdapter(Adapter);
    return Status;
  }

  // Interrupt registration (legacy INTx).
  RtlZeroMemory(&Intr, sizeof(Intr));
  Intr.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_INTERRUPT;
  Intr.Header.Revision = NDIS_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_1;
  Intr.Header.Size = sizeof(Intr);
  Intr.InterruptHandler = AerovNetInterruptIsr;
  Intr.InterruptDpcHandler = AerovNetInterruptDpc;

  Status = NdisMRegisterInterruptEx(MiniportAdapterHandle, Adapter, &Intr, &Adapter->InterruptHandle);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetCleanupAdapter(Adapter);
    return Status;
  }

  // Scatter-gather DMA.
  RtlZeroMemory(&DmaDesc, sizeof(DmaDesc));
  DmaDesc.Header.Type = NDIS_OBJECT_TYPE_SG_DMA_DESCRIPTION;
  DmaDesc.Header.Revision = NDIS_SG_DMA_DESCRIPTION_REVISION_1;
  DmaDesc.Header.Size = sizeof(DmaDesc);
  DmaDesc.Flags = NDIS_SG_DMA_64_BIT_ADDRESS;
  DmaDesc.MaximumPhysicalMapping = 0xFFFFFFFF;
  DmaDesc.ProcessSGListHandler = AerovNetProcessSgList;

  Status = NdisMRegisterScatterGatherDma(MiniportAdapterHandle, &DmaDesc, &Adapter->DmaHandle);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetCleanupAdapter(Adapter);
    return Status;
  }

  // Receive NBL pool.
  RtlZeroMemory(&PoolParams, sizeof(PoolParams));
  PoolParams.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
  PoolParams.Header.Revision = NDIS_NET_BUFFER_LIST_POOL_PARAMETERS_REVISION_1;
  PoolParams.Header.Size = sizeof(PoolParams);
  PoolParams.ProtocolId = NDIS_PROTOCOL_ID_DEFAULT;
  PoolParams.fAllocateNetBuffer = TRUE;

  Adapter->NblPool = NdisAllocateNetBufferListPool(MiniportAdapterHandle, &PoolParams);
  if (!Adapter->NblPool) {
    AerovNetCleanupAdapter(Adapter);
    return NDIS_STATUS_RESOURCES;
  }

  Status = AerovNetVirtioStart(Adapter);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetCleanupAdapter(Adapter);
    return Status;
  }

  // General attributes.
  RtlZeroMemory(&Gen, sizeof(Gen));
  Gen.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_ADAPTER_GENERAL_ATTRIBUTES;
  Gen.Header.Revision = NDIS_MINIPORT_ADAPTER_GENERAL_ATTRIBUTES_REVISION_2;
  Gen.Header.Size = sizeof(Gen);
  Gen.MediaType = NdisMedium802_3;
  Gen.PhysicalMediumType = NdisPhysicalMedium802_3;
  Gen.MtuSize = Adapter->Mtu;
  Gen.MaxXmitLinkSpeed = g_DefaultLinkSpeedBps;
  Gen.MaxRcvLinkSpeed = g_DefaultLinkSpeedBps;
  Gen.XmitLinkSpeed = g_DefaultLinkSpeedBps;
  Gen.RcvLinkSpeed = g_DefaultLinkSpeedBps;
  Gen.MediaConnectState = Adapter->LinkUp ? MediaConnectStateConnected : MediaConnectStateDisconnected;
  Gen.MediaDuplexState = MediaDuplexStateFull;
  Gen.LookaheadSize = Adapter->Mtu;
  Gen.MacAddressLength = ETH_LENGTH_OF_ADDRESS;
  Gen.PermanentMacAddress = Adapter->PermanentMac;
  Gen.CurrentMacAddress = Adapter->CurrentMac;
  Gen.SupportedPacketFilters = NDIS_PACKET_TYPE_DIRECTED | NDIS_PACKET_TYPE_MULTICAST | NDIS_PACKET_TYPE_ALL_MULTICAST |
                               NDIS_PACKET_TYPE_BROADCAST | NDIS_PACKET_TYPE_PROMISCUOUS;
  Gen.MaxMulticastListSize = NDIS_MAX_MULTICAST_LIST;
  Gen.MacOptions = NDIS_MAC_OPTION_COPY_LOOKAHEAD_DATA | NDIS_MAC_OPTION_NO_LOOPBACK;
  Gen.SupportedStatistics = NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_RCV | NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_XMIT |
                            NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_RCV | NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_XMIT;
  Gen.SupportedOidList = (PVOID)g_SupportedOids;
  Gen.SupportedOidListLength = sizeof(g_SupportedOids);

  Status = NdisMSetMiniportAttributes(MiniportAdapterHandle, (PNDIS_MINIPORT_ADAPTER_ATTRIBUTES)&Gen);
  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetCleanupAdapter(Adapter);
    return Status;
  }

  NdisAcquireSpinLock(&Adapter->Lock);
  Adapter->State = AerovNetAdapterRunning;
  NdisReleaseSpinLock(&Adapter->Lock);

  AerovNetIndicateLinkState(Adapter);

  return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetDriverUnload(_In_ PDRIVER_OBJECT DriverObject) {
  UNREFERENCED_PARAMETER(DriverObject);

  if (g_NdisDriverHandle) {
    NdisMDeregisterMiniportDriver(g_NdisDriverHandle);
    g_NdisDriverHandle = NULL;
  }
}

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath) {
  NDIS_STATUS Status;
  NDIS_MINIPORT_DRIVER_CHARACTERISTICS Ch;

  RtlZeroMemory(&Ch, sizeof(Ch));
  Ch.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_DRIVER_CHARACTERISTICS;
  Ch.Header.Revision = NDIS_MINIPORT_DRIVER_CHARACTERISTICS_REVISION_2;
  Ch.Header.Size = sizeof(Ch);

  Ch.MajorNdisVersion = 6;
  Ch.MinorNdisVersion = 20;
  Ch.MajorDriverVersion = 1;
  Ch.MinorDriverVersion = 0;
  Ch.InitializeHandlerEx = AerovNetMiniportInitializeEx;
  Ch.HaltHandlerEx = AerovNetMiniportHaltEx;
  Ch.PauseHandler = AerovNetMiniportPause;
  Ch.RestartHandler = AerovNetMiniportRestart;
  Ch.OidRequestHandler = AerovNetMiniportOidRequest;
  Ch.SendNetBufferListsHandler = AerovNetMiniportSendNetBufferLists;
  Ch.ReturnNetBufferListsHandler = AerovNetMiniportReturnNetBufferLists;
  Ch.CancelSendHandler = AerovNetMiniportCancelSend;
  Ch.DevicePnPEventNotifyHandler = AerovNetMiniportDevicePnPEventNotify;
  Ch.UnloadHandler = AerovNetDriverUnload;

  Status = NdisMRegisterMiniportDriver(DriverObject, RegistryPath, NULL, &Ch, &g_NdisDriverHandle);
  if (Status != NDIS_STATUS_SUCCESS) {
    g_NdisDriverHandle = NULL;
    return Status;
  }

  return STATUS_SUCCESS;
}
