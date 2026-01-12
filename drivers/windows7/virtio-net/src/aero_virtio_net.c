#include "../include/aero_virtio_net.h"

#include "virtio_pci_aero_layout_miniport.h"

#define AEROVNET_TAG 'tNvA'

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

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

static __forceinline virtio_bool_t AerovNetVirtqueueKickPrepareContractV1(_Inout_ virtqueue_split_t* Vq) {
  /*
   * Contract v1 uses "always notify" semantics (EVENT_IDX is not offered).
   *
   * Even if the device sets VIRTQ_USED_F_NO_NOTIFY, Aero drivers still notify
   * after publishing new available entries to keep behavior deterministic and
   * avoid relying on suppression bits that are out of scope for the contract.
   */
  if (Vq == NULL) {
    return VIRTIO_FALSE;
  }

  if (Vq->avail_idx == Vq->last_kick_avail) {
    return VIRTIO_FALSE;
  }

  if (Vq->event_idx != VIRTIO_FALSE) {
    /* If EVENT_IDX is enabled, respect the standard virtio suppression logic. */
    return virtqueue_split_kick_prepare(Vq);
  }

  /* Keep virtqueue bookkeeping consistent even when always-notify is used. */
  Vq->last_kick_avail = Vq->avail_idx;
  return VIRTIO_TRUE;
}

static USHORT AerovNetReadLe16FromPciCfg(_In_reads_bytes_(256) const UCHAR* Cfg, _In_ ULONG Offset) {
  USHORT V;

  V = 0;
  if (Offset + sizeof(V) > 256u) {
    return 0;
  }

  RtlCopyMemory(&V, Cfg + Offset, sizeof(V));
  return V;
}

static VOID AerovNetFreeTxRequestNoLock(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_TX_REQUEST* TxReq) {
  TxReq->State = AerovNetTxFree;
  TxReq->Cancelled = FALSE;
  TxReq->Nbl = NULL;
  TxReq->Nb = NULL;
  TxReq->SgList = NULL;
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

static BOOLEAN AerovNetExtractMemoryResource(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR* Desc, _Out_ PHYSICAL_ADDRESS* Start,
                                            _Out_ ULONG* Length) {
  USHORT Large;
  ULONGLONG Len;

  if (Start) {
    Start->QuadPart = 0;
  }
  if (Length) {
    *Length = 0;
  }

  if (!Desc || !Start || !Length) {
    return FALSE;
  }

  Len = 0;

  switch (Desc->Type) {
    case CmResourceTypeMemory:
      *Start = Desc->u.Memory.Start;
      *Length = Desc->u.Memory.Length;
      return TRUE;

    case CmResourceTypeMemoryLarge:
      /*
       * PCI MMIO above 4GiB may be reported as CmResourceTypeMemoryLarge.
       * The active union member depends on Desc->Flags.
       */
      Large = Desc->Flags & (CM_RESOURCE_MEMORY_LARGE_40 | CM_RESOURCE_MEMORY_LARGE_48 | CM_RESOURCE_MEMORY_LARGE_64);
      switch (Large) {
        case CM_RESOURCE_MEMORY_LARGE_40:
          *Start = Desc->u.Memory40.Start;
          Len = ((ULONGLONG)Desc->u.Memory40.Length40) << 8;
          break;
        case CM_RESOURCE_MEMORY_LARGE_48:
          *Start = Desc->u.Memory48.Start;
          Len = ((ULONGLONG)Desc->u.Memory48.Length48) << 16;
          break;
        case CM_RESOURCE_MEMORY_LARGE_64:
          *Start = Desc->u.Memory64.Start;
          Len = ((ULONGLONG)Desc->u.Memory64.Length64) << 32;
          break;
        default:
          return FALSE;
      }

      if (Len > 0xFFFFFFFFull) {
        return FALSE;
      }
      *Length = (ULONG)Len;
      return TRUE;

    default:
      return FALSE;
  }
}

static NDIS_STATUS AerovNetParseResources(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ PNDIS_RESOURCE_LIST Resources) {
  ULONG I;
  NDIS_STATUS Status;
  NTSTATUS NtStatus;
  UINT64 Bar0Base;
  const UCHAR* PciCfg;
  ULONG BytesRead;
  UINT32 Bar0Low;
  UINT32 Bar0High;
  UCHAR InterruptPin;

  Adapter->Bar0Va = NULL;
  Adapter->Bar0Length = 0;
  Adapter->Bar0Pa.QuadPart = 0;
  RtlZeroMemory(&Adapter->Vdev, sizeof(Adapter->Vdev));

  if (!Resources) {
    return NDIS_STATUS_RESOURCES;
  }

  // Prefer matching the assigned memory range (CmResourceTypeMemory or
  // CmResourceTypeMemoryLarge) against BAR0 from PCI config space (BAR0 is
  // required by the AERO-W7-VIRTIO contract).
  RtlZeroMemory(Adapter->PciCfgSpace, sizeof(Adapter->PciCfgSpace));
  BytesRead = NdisMGetBusData(Adapter->MiniportAdapterHandle, PCI_WHICHSPACE_CONFIG, Adapter->PciCfgSpace, 0, sizeof(Adapter->PciCfgSpace));
  if (BytesRead != sizeof(Adapter->PciCfgSpace)) {
    return NDIS_STATUS_FAILURE;
  }
  PciCfg = Adapter->PciCfgSpace;

  // Enforce contract v1 identity (VEN/DEV/REV) using the PCI config snapshot.
  if (AerovNetReadLe16FromPciCfg(PciCfg, 0x00) != AEROVNET_VENDOR_ID ||
      AerovNetReadLe16FromPciCfg(PciCfg, 0x02) != (USHORT)AEROVNET_PCI_DEVICE_ID || PciCfg[0x08] != AEROVNET_PCI_REVISION_ID) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  // Contract v1: INTx on INTA#.
  InterruptPin = PciCfg[0x3D];
  if (InterruptPin != 0x01u) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  // Contract v1: BAR0 is MMIO and 64-bit.
  Bar0Low = 0;
  Bar0High = 0;
  RtlCopyMemory(&Bar0Low, PciCfg + 0x10, sizeof(Bar0Low));
  RtlCopyMemory(&Bar0High, PciCfg + 0x14, sizeof(Bar0High));
  if ((Bar0Low & 0x1u) != 0 || (Bar0Low & 0x6u) != 0x4u) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Bar0Base = (UINT64)(Bar0Low & ~0xFu) | ((UINT64)Bar0High << 32);

  for (I = 0; I < Resources->Count; I++) {
    PCM_PARTIAL_RESOURCE_DESCRIPTOR Desc = &Resources->PartialDescriptors[I];
    PHYSICAL_ADDRESS Start;
    ULONG Length;

    if (!AerovNetExtractMemoryResource(Desc, &Start, &Length)) {
      continue;
    }
    if (Length < AEROVNET_BAR0_MIN_LEN) {
      continue;
    }
    if (Start.QuadPart != Bar0Base) {
      continue;
    }

    Adapter->Bar0Pa = Start;
    Adapter->Bar0Length = Length;
    break;
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

  NtStatus = VirtioPciModernMiniportInit(&Adapter->Vdev, Adapter->Bar0Va, Adapter->Bar0Length, PciCfg, sizeof(Adapter->PciCfgSpace));
  if (!NT_SUCCESS(NtStatus)) {
    NdisMUnmapIoSpace(Adapter->MiniportAdapterHandle, Adapter->Bar0Va, Adapter->Bar0Length);
    Adapter->Bar0Va = NULL;
    Adapter->Bar0Length = 0;
    Adapter->Bar0Pa.QuadPart = 0;
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Adapter->Vdev.QueueNotifyAddrCache = Adapter->QueueNotifyAddrCache;
  Adapter->Vdev.QueueNotifyAddrCacheCount = (USHORT)RTL_NUMBER_OF(Adapter->QueueNotifyAddrCache);

  if (!AeroVirtioValidateContractV1Bar0Layout(&Adapter->Vdev)) {
    NdisMUnmapIoSpace(Adapter->MiniportAdapterHandle, Adapter->Bar0Va, Adapter->Bar0Length);
    Adapter->Bar0Va = NULL;
    Adapter->Bar0Length = 0;
    Adapter->Bar0Pa.QuadPart = 0;
    RtlZeroMemory(&Adapter->Vdev, sizeof(Adapter->Vdev));
    return NDIS_STATUS_NOT_SUPPORTED;
  }

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
    if (Rx->BufferBytes != 0) {
      MmFreeContiguousMemorySpecifyCache(Rx->BufferVa, Rx->BufferBytes, MmCached);
    }
    Rx->BufferVa = NULL;
    Rx->BufferBytes = 0;
    Rx->BufferPa.QuadPart = 0;
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
    if (Adapter->TxHeaderBlockBytes != 0) {
      MmFreeContiguousMemorySpecifyCache(Adapter->TxHeaderBlockVa, Adapter->TxHeaderBlockBytes, MmCached);
    }
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

static VOID AerovNetFreeVq(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_VQ* Vq) {
  if (!Vq) {
    return;
  }

  virtqueue_split_destroy(&Vq->Vq);

  if (Adapter != NULL) {
    virtqueue_split_free_ring(&Adapter->VirtioOps, &Adapter->VirtioOpsCtx, &Vq->RingDma);
  } else {
    Vq->RingDma.vaddr = NULL;
    Vq->RingDma.paddr = 0;
    Vq->RingDma.size = 0;
  }

  Vq->QueueIndex = 0;
  Vq->QueueSize = 0;
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

  AerovNetFreeVq(Adapter, &Adapter->RxVq);
  AerovNetFreeVq(Adapter, &Adapter->TxVq);

  if (Adapter->Bar0Va) {
    NdisMUnmapIoSpace(Adapter->MiniportAdapterHandle, Adapter->Bar0Va, Adapter->Bar0Length);
    Adapter->Bar0Va = NULL;
    Adapter->Bar0Length = 0;
    Adapter->Bar0Pa.QuadPart = 0;
  }
  RtlZeroMemory(&Adapter->Vdev, sizeof(Adapter->Vdev));

  NdisFreeSpinLock(&Adapter->Lock);

  ExFreePoolWithTag(Adapter, AEROVNET_TAG);
}

static VOID AerovNetFillRxQueueLocked(_Inout_ AEROVNET_ADAPTER* Adapter) {
  BOOLEAN Notify = FALSE;

  while (!IsListEmpty(&Adapter->RxFreeList)) {
    PLIST_ENTRY Entry;
    AEROVNET_RX_BUFFER* Rx;
    virtio_sg_entry_t Sg[2];
    uint16_t Head;
    virtio_bool_t UseIndirect;
    int VqRes;

    // Each receive buffer is posted as a header + payload descriptor chain.
    if (Adapter->RxVq.QueueSize == 0) {
      break;
    }

    Entry = RemoveHeadList(&Adapter->RxFreeList);
    Rx = CONTAINING_RECORD(Entry, AEROVNET_RX_BUFFER, Link);

    Rx->Indicated = FALSE;

    Sg[0].addr = (uint64_t)Rx->BufferPa.QuadPart;
    Sg[0].len = (uint32_t)sizeof(VIRTIO_NET_HDR);
    Sg[0].device_writes = VIRTIO_TRUE;

    Sg[1].addr = (uint64_t)Rx->BufferPa.QuadPart + (uint64_t)sizeof(VIRTIO_NET_HDR);
    Sg[1].len = (uint32_t)(Rx->BufferBytes - sizeof(VIRTIO_NET_HDR));
    Sg[1].device_writes = VIRTIO_TRUE;

    UseIndirect = (Adapter->RxVq.Vq.indirect_desc != VIRTIO_FALSE) ? VIRTIO_TRUE : VIRTIO_FALSE;

    Head = 0;
    VqRes = virtqueue_split_add_sg(&Adapter->RxVq.Vq, Sg, 2, Rx, UseIndirect, &Head);
    if (VqRes != VIRTIO_OK) {
      InsertHeadList(&Adapter->RxFreeList, &Rx->Link);
      break;
    }

    UNREFERENCED_PARAMETER(Head);
    Notify = TRUE;
  }

  if (Notify) {
    if (AerovNetVirtqueueKickPrepareContractV1(&Adapter->RxVq.Vq) != VIRTIO_FALSE) {
      KeMemoryBarrier();
      if (!Adapter->SurpriseRemoved) {
        VirtioPciNotifyQueue(&Adapter->Vdev, Adapter->RxVq.QueueIndex);
      }
    }
  }
}

static VOID AerovNetFlushTxPendingLocked(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PLIST_ENTRY CompleteTxReqs,
                                         _Inout_ PNET_BUFFER_LIST* CompleteNblHead, _Inout_ PNET_BUFFER_LIST* CompleteNblTail) {
  virtio_sg_entry_t Sg[AEROVNET_MAX_TX_SG_ELEMENTS + 1];
  BOOLEAN Notified = FALSE;

  while (!IsListEmpty(&Adapter->TxPendingList)) {
    AEROVNET_TX_REQUEST* TxReq;
    uint16_t Needed;
    ULONG I;
    uint16_t Head;
    virtio_bool_t UseIndirect;
    int VqRes;

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

    Needed = (uint16_t)(TxReq->SgList->NumberOfElements + 1);

    Sg[0].addr = (uint64_t)TxReq->HeaderPa.QuadPart;
    Sg[0].len = (uint32_t)sizeof(VIRTIO_NET_HDR);
    Sg[0].device_writes = VIRTIO_FALSE;

    for (I = 0; I < TxReq->SgList->NumberOfElements; I++) {
      Sg[1 + I].addr = (uint64_t)TxReq->SgList->Elements[I].Address.QuadPart;
      Sg[1 + I].len = (uint32_t)TxReq->SgList->Elements[I].Length;
      Sg[1 + I].device_writes = VIRTIO_FALSE;
    }

    UseIndirect = (Adapter->TxVq.Vq.indirect_desc != VIRTIO_FALSE && Needed > 1u) ? VIRTIO_TRUE : VIRTIO_FALSE;
    Head = 0;
    VqRes = virtqueue_split_add_sg(&Adapter->TxVq.Vq, Sg, Needed, TxReq, UseIndirect, &Head);
    if (VqRes != VIRTIO_OK) {
      break;
    }

    RemoveEntryList(&TxReq->Link);
    UNREFERENCED_PARAMETER(Head);

    TxReq->State = AerovNetTxSubmitted;
    InsertTailList(&Adapter->TxSubmittedList, &TxReq->Link);
    Notified = TRUE;
  }

  if (Notified) {
    if (AerovNetVirtqueueKickPrepareContractV1(&Adapter->TxVq.Vq) != VIRTIO_FALSE) {
      KeMemoryBarrier();
      if (!Adapter->SurpriseRemoved) {
        VirtioPciNotifyQueue(&Adapter->Vdev, Adapter->TxVq.QueueIndex);
      }
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
  // Allocate more buffers than the ring can hold so we can keep rxq full even
  // while NDIS is still holding previously indicated NBLs.
  Adapter->RxBufferCount = (ULONG)Adapter->RxVq.QueueSize * 2;

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

static VOID AerovNetDisableQueueMsixVector(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ USHORT QueueIndex) {
  KIRQL OldIrql;

  if (!Adapter || !Adapter->Vdev.CommonCfg) {
    return;
  }

  KeAcquireSpinLock(&Adapter->Vdev.CommonCfgLock, &OldIrql);

  WRITE_REGISTER_USHORT((volatile USHORT*)&Adapter->Vdev.CommonCfg->queue_select, QueueIndex);
  KeMemoryBarrier();
  WRITE_REGISTER_USHORT((volatile USHORT*)&Adapter->Vdev.CommonCfg->queue_msix_vector, VIRTIO_PCI_MSI_NO_VECTOR);
  KeMemoryBarrier();

  KeReleaseSpinLock(&Adapter->Vdev.CommonCfgLock, OldIrql);
}

static NDIS_STATUS AerovNetSetupVq(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_VQ* Vq, _In_ USHORT QueueIndex,
                                  _In_ USHORT ExpectedQueueSize, _In_ USHORT IndirectMaxDesc) {
  USHORT QueueSize;
  NTSTATUS NtStatus;
  int VqRes;
  virtio_bool_t UseIndirect;
  volatile UINT16* NotifyAddr;
  volatile UINT16* ExpectedNotifyAddr;
  ULONGLONG NotifyOffset;
  UINT64 DescPa;
  UINT64 AvailPa;
  UINT64 UsedPa;

  if (!Adapter || !Vq) {
    return NDIS_STATUS_FAILURE;
  }

  RtlZeroMemory(Vq, sizeof(*Vq));
  Vq->QueueIndex = QueueIndex;

  QueueSize = VirtioPciGetQueueSize(&Adapter->Vdev, QueueIndex);
  if (QueueSize == 0) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }
  if (QueueSize != ExpectedQueueSize) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  // Contract v1: notify_off_multiplier=4 and queue_notify_off(q)=q.
  NotifyAddr = NULL;
  NtStatus = VirtioPciGetQueueNotifyAddress(&Adapter->Vdev, QueueIndex, &NotifyAddr);
  if (!NT_SUCCESS(NtStatus) || NotifyAddr == NULL) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  NotifyOffset = (ULONGLONG)QueueIndex * (ULONGLONG)Adapter->Vdev.NotifyOffMultiplier;
  ExpectedNotifyAddr = (volatile UINT16*)((volatile UCHAR*)Adapter->Vdev.NotifyBase + NotifyOffset);
  if (NotifyAddr != ExpectedNotifyAddr) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }
  if (QueueIndex < Adapter->Vdev.QueueNotifyAddrCacheCount) {
    Adapter->QueueNotifyAddrCache[QueueIndex] = NotifyAddr;
  }

  Vq->QueueSize = QueueSize;

  VqRes = virtqueue_split_alloc_ring(&Adapter->VirtioOps, &Adapter->VirtioOpsCtx, QueueSize, 16, VIRTIO_FALSE, &Vq->RingDma);
  if (VqRes != VIRTIO_OK) {
    return NDIS_STATUS_RESOURCES;
  }

  UseIndirect = (IndirectMaxDesc != 0) ? VIRTIO_TRUE : VIRTIO_FALSE;
  VqRes = virtqueue_split_init(&Vq->Vq,
                               &Adapter->VirtioOps,
                               &Adapter->VirtioOpsCtx,
                               QueueIndex,
                               QueueSize,
                               16,
                               &Vq->RingDma,
                               VIRTIO_FALSE,
                               UseIndirect,
                               (uint16_t)IndirectMaxDesc);

  if (VqRes != VIRTIO_OK && UseIndirect != VIRTIO_FALSE) {
    // Indirect is optional; fall back to direct descriptors if we couldn't allocate tables.
    virtqueue_split_destroy(&Vq->Vq);
    VqRes = virtqueue_split_init(&Vq->Vq,
                                 &Adapter->VirtioOps,
                                 &Adapter->VirtioOpsCtx,
                                 QueueIndex,
                                 QueueSize,
                                 16,
                                 &Vq->RingDma,
                                 VIRTIO_FALSE,
                                 VIRTIO_FALSE,
                                 0);
  }

  if (VqRes != VIRTIO_OK) {
    return NDIS_STATUS_RESOURCES;
  }

  // Disable MSI-X for this queue; INTx/ISR is required by contract v1.
  AerovNetDisableQueueMsixVector(Adapter, QueueIndex);

  DescPa = Vq->RingDma.paddr + (UINT64)((PUCHAR)Vq->Vq.desc - (PUCHAR)Vq->RingDma.vaddr);
  AvailPa = Vq->RingDma.paddr + (UINT64)((PUCHAR)Vq->Vq.avail - (PUCHAR)Vq->RingDma.vaddr);
  UsedPa = Vq->RingDma.paddr + (UINT64)((PUCHAR)Vq->Vq.used - (PUCHAR)Vq->RingDma.vaddr);

  NtStatus = VirtioPciSetupQueue(&Adapter->Vdev, QueueIndex, DescPa, AvailPa, UsedPa);
  if (!NT_SUCCESS(NtStatus)) {
    return NDIS_STATUS_FAILURE;
  }

  return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetVirtioStart(_Inout_ AEROVNET_ADAPTER* Adapter) {
  NDIS_STATUS Status;
  UCHAR Mac[ETH_LENGTH_OF_ADDRESS];
  USHORT LinkStatus;
  USHORT MaxPairs;
  ULONGLONG RequiredFeatures;
  ULONGLONG WantedFeatures;
  ULONGLONG NegotiatedFeatures;
  NTSTATUS NtStatus;
  USHORT RxIndirectMaxDesc;
  USHORT TxIndirectMaxDesc;

  if (!Adapter || !Adapter->Vdev.CommonCfg || !Adapter->Vdev.DeviceCfg || !Adapter->Vdev.IsrStatus || !Adapter->Vdev.NotifyBase) {
    return NDIS_STATUS_FAILURE;
  }

  /*
   * Contract v1 ring invariants (docs/windows7-virtio-driver-contract.md §2.3):
   * - MUST offer INDIRECT_DESC
   * - EVENT_IDX/PACKED are not negotiated by the driver (split ring, always-notify)
   *
   * Some hypervisors (notably QEMU) may still advertise EVENT_IDX/PACKED even
   * when the guest chooses not to negotiate them, so do not fail init just
   * because those bits are present in the offered feature set.
   */
  Adapter->HostFeatures = VirtioPciReadDeviceFeatures(&Adapter->Vdev);

  // Contract v1 features (docs/windows7-virtio-driver-contract.md §3.2.3):
  // - required: VERSION_1 + INDIRECT_DESC + MAC + STATUS
  RequiredFeatures = VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS | AEROVNET_FEATURE_RING_INDIRECT_DESC;
  WantedFeatures = 0;
  NegotiatedFeatures = 0;

  NtStatus = VirtioPciNegotiateFeatures(&Adapter->Vdev, RequiredFeatures, WantedFeatures, &NegotiatedFeatures);
  if (!NT_SUCCESS(NtStatus)) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Adapter->GuestFeatures = (UINT64)NegotiatedFeatures;

  // Disable MSI-X config interrupt vector; INTx/ISR is required by contract v1.
  WRITE_REGISTER_USHORT((volatile USHORT*)&Adapter->Vdev.CommonCfg->msix_config, VIRTIO_PCI_MSI_NO_VECTOR);
  KeMemoryBarrier();

  // Read virtio-net device config (MAC + link status).
  RtlZeroMemory(Mac, sizeof(Mac));
  NtStatus = VirtioPciReadDeviceConfig(&Adapter->Vdev, 0, Mac, sizeof(Mac));
  if (!NT_SUCCESS(NtStatus)) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return NDIS_STATUS_FAILURE;
  }
  RtlCopyMemory(Adapter->PermanentMac, Mac, ETH_LENGTH_OF_ADDRESS);
  RtlCopyMemory(Adapter->CurrentMac, Mac, ETH_LENGTH_OF_ADDRESS);

  LinkStatus = 0;
  NtStatus = VirtioPciReadDeviceConfig(&Adapter->Vdev, ETH_LENGTH_OF_ADDRESS, &LinkStatus, sizeof(LinkStatus));
  if (NT_SUCCESS(NtStatus)) {
    Adapter->LinkUp = (LinkStatus & VIRTIO_NET_S_LINK_UP) ? TRUE : FALSE;
  } else {
    Adapter->LinkUp = TRUE;
  }

  MaxPairs = 0;
  NtStatus = VirtioPciReadDeviceConfig(&Adapter->Vdev, 0x08, &MaxPairs, sizeof(MaxPairs));
  if (NT_SUCCESS(NtStatus) && MaxPairs != 1) {
    DbgPrint("aero_virtio_net: max_virtqueue_pairs=%hu (expected 1)\n", MaxPairs);
  }
  RxIndirectMaxDesc = (Adapter->GuestFeatures & AEROVNET_FEATURE_RING_INDIRECT_DESC) ? 2 : 0;
  TxIndirectMaxDesc = (Adapter->GuestFeatures & AEROVNET_FEATURE_RING_INDIRECT_DESC) ? (USHORT)(AEROVNET_MAX_TX_SG_ELEMENTS + 1) : 0;

  // Virtqueues: 0 = RX, 1 = TX.
  if (VirtioPciGetNumQueues(&Adapter->Vdev) < 2) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Status = AerovNetSetupVq(Adapter, &Adapter->RxVq, 0, 256, RxIndirectMaxDesc);
  if (Status != NDIS_STATUS_SUCCESS) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return Status;
  }

  Status = AerovNetSetupVq(Adapter, &Adapter->TxVq, 1, 256, TxIndirectMaxDesc);
  if (Status != NDIS_STATUS_SUCCESS) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return Status;
  }

  // Allocate packet buffers.
  Adapter->Mtu = AEROVNET_MTU_DEFAULT;
  Adapter->MaxFrameSize = Adapter->Mtu + 14;

  Adapter->RxBufferDataBytes = 2048;
  Adapter->RxBufferTotalBytes = sizeof(VIRTIO_NET_HDR) + Adapter->RxBufferDataBytes;

  Status = AerovNetAllocateRxResources(Adapter);
  if (Status != NDIS_STATUS_SUCCESS) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return Status;
  }

  Status = AerovNetAllocateTxResources(Adapter);
  if (Status != NDIS_STATUS_SUCCESS) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return Status;
  }

  // Pre-post RX buffers.
  NdisAcquireSpinLock(&Adapter->Lock);
  AerovNetFillRxQueueLocked(Adapter);
  NdisReleaseSpinLock(&Adapter->Lock);

  VirtioPciAddStatus(&Adapter->Vdev, VIRTIO_STATUS_DRIVER_OK);
  return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetVirtioStop(_Inout_ AEROVNET_ADAPTER* Adapter) {
  LIST_ENTRY AbortTxReqs;
  PNET_BUFFER_LIST CompleteHead;
  PNET_BUFFER_LIST CompleteTail;
  BOOLEAN SurpriseRemoved;

  if (!Adapter) {
    return;
  }

  NdisAcquireSpinLock(&Adapter->Lock);
  SurpriseRemoved = Adapter->SurpriseRemoved;
  NdisReleaseSpinLock(&Adapter->Lock);

  // Stop the device first to prevent further DMA/interrupts. After surprise
  // removal, the device may no longer be accessible and any BAR MMIO access can
  // fault/hang on real hardware or strict virtual PCI implementations.
  if (SurpriseRemoved) {
    DbgPrint("aero_virtio_net: stop: SurpriseRemoved=TRUE; skipping virtio MMIO reset\n");
  } else {
    DbgPrint("aero_virtio_net: stop: resetting virtio device\n");
    VirtioPciResetDevice(&Adapter->Vdev);
  }

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

  AerovNetFreeVq(Adapter, &Adapter->RxVq);
  AerovNetFreeVq(Adapter, &Adapter->TxVq);
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

  if (Adapter->State == AerovNetAdapterStopped || Adapter->SurpriseRemoved) {
    return FALSE;
  }

  Isr = VirtioPciReadIsr(&Adapter->Vdev);
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

  if (Adapter->State == AerovNetAdapterStopped || Adapter->SurpriseRemoved) {
    NdisReleaseSpinLock(&Adapter->Lock);
    return;
  }

  // TX completions.
  for (;;) {
    PVOID Cookie;
    AEROVNET_TX_REQUEST* TxReq;

    Cookie = NULL;

    if (Adapter->TxVq.QueueSize == 0) {
      break;
    }

    if (virtqueue_split_pop_used(&Adapter->TxVq.Vq, &Cookie, NULL) == VIRTIO_FALSE) {
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
    uint32_t UsedLen;
    AEROVNET_RX_BUFFER* Rx;
    ULONG PayloadLen;

    Cookie = NULL;
    UsedLen = 0;

    if (Adapter->RxVq.QueueSize == 0) {
      break;
    }

    if (virtqueue_split_pop_used(&Adapter->RxVq.Vq, &Cookie, &UsedLen) == VIRTIO_FALSE) {
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

  // Link state change handling (config interrupt).
  if ((Isr & 0x2) != 0) {
    USHORT LinkStatus;
    NTSTATUS NtStatus;

    if (!Adapter->SurpriseRemoved) {
      LinkStatus = 0;
      NtStatus = VirtioPciReadDeviceConfig(&Adapter->Vdev, ETH_LENGTH_OF_ADDRESS, &LinkStatus, sizeof(LinkStatus));
      if (NT_SUCCESS(NtStatus)) {
        NewLinkUp = (LinkStatus & VIRTIO_NET_S_LINK_UP) ? TRUE : FALSE;
        if (NewLinkUp != Adapter->LinkUp) {
          Adapter->LinkUp = NewLinkUp;
          LinkChanged = TRUE;
        }
      }
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
  virtio_sg_entry_t Sg[AEROVNET_MAX_TX_SG_ELEMENTS + 1];
  ULONG ElemCount;
  uint16_t Needed;
  ULONG I;
  int VqRes;
  uint16_t Head;
  virtio_bool_t UseIndirect;
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
  Needed = (uint16_t)(ElemCount + 1);

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

    Sg[0].addr = (uint64_t)TxReq->HeaderPa.QuadPart;
    Sg[0].len = (uint32_t)sizeof(VIRTIO_NET_HDR);
    Sg[0].device_writes = VIRTIO_FALSE;

    for (I = 0; I < ElemCount; I++) {
      Sg[1 + I].addr = (uint64_t)ScatterGatherList->Elements[I].Address.QuadPart;
      Sg[1 + I].len = (uint32_t)ScatterGatherList->Elements[I].Length;
      Sg[1 + I].device_writes = VIRTIO_FALSE;
    }

    UseIndirect = (Adapter->TxVq.Vq.indirect_desc != VIRTIO_FALSE && Needed > 1u) ? VIRTIO_TRUE : VIRTIO_FALSE;
    Head = 0;
    VqRes = virtqueue_split_add_sg(&Adapter->TxVq.Vq, Sg, Needed, TxReq, UseIndirect, &Head);
    if (VqRes != VIRTIO_OK) {
      // No descriptors yet; queue it for later retry (DPC will flush).
      TxReq->State = AerovNetTxPendingSubmit;
      InsertTailList(&Adapter->TxPendingList, &TxReq->Link);
    } else {
      UNREFERENCED_PARAMETER(Head);
      TxReq->State = AerovNetTxSubmitted;
      InsertTailList(&Adapter->TxSubmittedList, &TxReq->Link);
      if (AerovNetVirtqueueKickPrepareContractV1(&Adapter->TxVq.Vq) != VIRTIO_FALSE) {
        KeMemoryBarrier();
        if (!Adapter->SurpriseRemoved) {
          VirtioPciNotifyQueue(&Adapter->Vdev, Adapter->TxVq.QueueIndex);
        }
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
        NDIS_STATUS TxStatus = (Adapter->State == AerovNetAdapterPaused) ? NDIS_STATUS_PAUSED : NDIS_STATUS_RESET_IN_PROGRESS;
        AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, TxStatus, &CompleteHead, &CompleteTail);
        NdisReleaseSpinLock(&Adapter->Lock);
        continue;
      }

      // Contract v1 frame size rules: drop undersized/oversized frames.
      // Complete the send successfully (Ethernet has no delivery guarantee).
      {
        ULONG FrameLen = NET_BUFFER_DATA_LENGTH(Nb);
        if (FrameLen < 14 || FrameLen > 1514) {
        Adapter->StatTxErrors++;
        AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, NDIS_STATUS_SUCCESS, &CompleteHead, &CompleteTail);
        NdisReleaseSpinLock(&Adapter->Lock);
        continue;
      }
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
    // Set this flag first without taking the adapter lock. The surprise removal
    // callback can race with DPC/ISR contexts; setting the flag early allows
    // other paths to quickly stop issuing virtio BAR MMIO (e.g. queue notify).
    Adapter->SurpriseRemoved = TRUE;

    NdisAcquireSpinLock(&Adapter->Lock);
    Adapter->State = AerovNetAdapterStopped;

    // Once SurpriseRemoved is set, the device may have already disappeared.
    // Clear BAR-backed pointers/caches so any accidental virtio access becomes a
    // no-op instead of touching unmapped MMIO.
    Adapter->Vdev.CommonCfg = NULL;
    Adapter->Vdev.NotifyBase = NULL;
    Adapter->Vdev.IsrStatus = NULL;
    Adapter->Vdev.DeviceCfg = NULL;
    Adapter->Vdev.QueueNotifyAddrCache = NULL;
    Adapter->Vdev.QueueNotifyAddrCacheCount = 0;
    RtlZeroMemory(Adapter->QueueNotifyAddrCache, sizeof(Adapter->QueueNotifyAddrCache));
    NdisReleaseSpinLock(&Adapter->Lock);

    // On surprise removal, the device may no longer be accessible. Avoid any
    // further virtio BAR MMIO access here; full software cleanup happens in
    // HaltEx (PASSIVE_LEVEL).
#if DBG
    DbgPrint("aero_virtio_net: pnp: SurpriseRemoved=TRUE; skipping hardware quiesce (BAR0 MMIO may be invalid)\n");
#endif
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

  virtio_os_ndis_get_ops(&Adapter->VirtioOps);
  Adapter->VirtioOpsCtx.pool_tag = AEROVNET_TAG;

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

  // Interrupt registration (INTx).
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
