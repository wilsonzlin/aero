#include "../include/aero_virtio_net.h"
#include "../include/aero_virtio_net_diag.h"
#include "../include/aero_virtio_net_offload.h"

#define VIRTIO_NET_HDR_OFFLOAD_USE_EXTERNAL_HDR 1
#include "../include/virtio_net_hdr_offload.h"

#include "virtio_pci_aero_layout_miniport.h"

#define AEROVNET_TAG 'tNvA'

C_ASSERT(sizeof(VIRTIO_NET_HDR) == sizeof(AEROVNET_VIRTIO_NET_HDR));

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

static NDIS_HANDLE g_NdisDriverHandle = NULL;
static NDIS_HANDLE g_NdisDeviceHandle = NULL;
static PDEVICE_OBJECT g_NdisDeviceObject = NULL;
static NDIS_SPIN_LOCK g_DiagLock;
static BOOLEAN g_DiagLockInitialized = FALSE;
static AEROVNET_ADAPTER* g_DiagAdapter = NULL;
static PDRIVER_DISPATCH g_DiagMajorFunctions[IRP_MJ_MAXIMUM_FUNCTION + 1];

// Allow System/Admin full access, Everyone read (diagnostic-only interface).
static const WCHAR g_AerovNetDiagSddl[] = L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GR;;;WD)";

// `\\.\AeroVirtioNetDiag` user-mode diagnostics interface (read-only).
#define AEROVNET_DIAG_DEVICE_NAME L"\\Device\\AeroVirtioNetDiag"
#define AEROVNET_DIAG_SYMBOLIC_NAME L"\\DosDevices\\AeroVirtioNetDiag"

C_ASSERT(AEROVNET_DIAG_IOCTL_QUERY == CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800u, METHOD_BUFFERED, FILE_READ_ACCESS));
C_ASSERT(sizeof(AEROVNET_DIAG_INFO) <= 256);

static NTSTATUS AerovNetDiagDispatchCreateClose(_In_ PDEVICE_OBJECT DeviceObject, _Inout_ PIRP Irp);
static NTSTATUS AerovNetDiagDispatchDeviceControl(_In_ PDEVICE_OBJECT DeviceObject, _Inout_ PIRP Irp);
static NTSTATUS AerovNetDiagDispatchDefault(_In_ PDEVICE_OBJECT DeviceObject, _Inout_ PIRP Irp);

static BOOLEAN AerovNetInterruptIsr(_In_ NDIS_HANDLE MiniportInterruptContext, _Out_ PBOOLEAN QueueDefaultInterruptDpc,
                                   _Out_ PULONG TargetProcessors);
static VOID AerovNetInterruptDpc(_In_ NDIS_HANDLE MiniportInterruptContext, _In_ PVOID MiniportDpcContext, _In_ PULONG NdisReserved1,
                                 _In_ PULONG NdisReserved2);

static VOID AerovNetDiagDetachAdapter(_In_ AEROVNET_ADAPTER* Adapter);
static VOID AerovNetDiagAttachAdapter(_In_ AEROVNET_ADAPTER* Adapter);

#if DBG
static volatile LONG g_AerovNetDbgTxCancelBeforeSg = 0;
static volatile LONG g_AerovNetDbgTxCancelAfterSg = 0;
static volatile LONG g_AerovNetDbgTxCancelAfterSubmit = 0;
static volatile LONG g_AerovNetDbgTxTcpCsumOffload = 0;
static volatile LONG g_AerovNetDbgTxTcpCsumFallback = 0;
static volatile LONG g_AerovNetDbgTxUdpCsumOffload = 0;
static volatile LONG g_AerovNetDbgTxUdpCsumFallback = 0;
#endif
static VOID AerovNetFreeCtrlPendingRequests(_Inout_ AEROVNET_ADAPTER* Adapter);
static NDIS_STATUS AerovNetCtrlVlanUpdate(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ BOOLEAN Add, _In_ USHORT VlanId);

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

    // Offloads (NDIS 6.20).
    OID_TCP_OFFLOAD_HARDWARE_CAPABILITIES,
    OID_TCP_OFFLOAD_CURRENT_CONFIG,
    OID_TCP_OFFLOAD_PARAMETERS,
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

static __forceinline USHORT AerovNetReadBe16(_In_reads_bytes_(2) const UCHAR* P) {
  return (USHORT)(((USHORT)P[0] << 8) | (USHORT)P[1]);
}

static __forceinline ULONG AerovNetReadBe32(_In_reads_bytes_(4) const UCHAR* P) {
  return ((ULONG)P[0] << 24) | ((ULONG)P[1] << 16) | ((ULONG)P[2] << 8) | (ULONG)P[3];
}

static __forceinline VOID AerovNetWriteBe16(_Out_writes_bytes_(2) UCHAR* P, _In_ USHORT V) {
  P[0] = (UCHAR)((V >> 8) & 0xFF);
  P[1] = (UCHAR)(V & 0xFF);
}

static __forceinline VOID AerovNetWriteBe32(_Out_writes_bytes_(4) UCHAR* P, _In_ ULONG V) {
  P[0] = (UCHAR)((V >> 24) & 0xFF);
  P[1] = (UCHAR)((V >> 16) & 0xFF);
  P[2] = (UCHAR)((V >> 8) & 0xFF);
  P[3] = (UCHAR)(V & 0xFF);
}

#define AEROVNET_ETHERTYPE_IPV4 0x0800u
#define AEROVNET_ETHERTYPE_IPV6 0x86DDu
#define AEROVNET_ETHERTYPE_VLAN 0x8100u
#define AEROVNET_ETHERTYPE_QINQ 0x88A8u
#define AEROVNET_ETHERTYPE_VLAN_9100 0x9100u

typedef enum _AEROVNET_L3_TYPE {
  AerovNetL3None = 0,
  AerovNetL3Ipv4,
  AerovNetL3Ipv6,
} AEROVNET_L3_TYPE;

typedef enum _AEROVNET_L4_TYPE {
  AerovNetL4None = 0,
  AerovNetL4Tcp,
  AerovNetL4Udp,
} AEROVNET_L4_TYPE;

typedef struct _AEROVNET_PACKET_INFO {
  AEROVNET_L3_TYPE L3;
  AEROVNET_L4_TYPE L4;
  USHORT L2Len;
  USHORT L3Offset;
  USHORT L4Offset;
  USHORT L4Len;
  USHORT L4CsumOffset; // Offset of checksum field within the L4 header.
  USHORT Ipv4HeaderLen;
  UCHAR IpProtocol; // TCP=6, UDP=17
  UCHAR SrcAddr[16]; // IPv4 uses first 4 bytes
  UCHAR DstAddr[16]; // IPv4 uses first 4 bytes
} AEROVNET_PACKET_INFO;

// One's complement checksum accumulator (network byte order, 16-bit words).
typedef struct _AEROVNET_CSUM_STATE {
  ULONG Sum;
  BOOLEAN Odd;
  UCHAR OddByte;
} AEROVNET_CSUM_STATE;

static VOID AerovNetCsumAccumulateBytes(_Inout_ AEROVNET_CSUM_STATE* St, _In_reads_bytes_(Len) const UCHAR* Data, _In_ ULONG Len) {
  ULONG I;

  if (!St || !Data || Len == 0) {
    return;
  }

  I = 0;

  if (St->Odd) {
    // Consume the first byte to complete the odd trailing byte from the previous chunk.
    St->Sum += ((ULONG)St->OddByte << 8) | (ULONG)Data[0];
    St->Odd = FALSE;
    St->OddByte = 0;
    I = 1;
  }

  for (; I + 1 < Len; I += 2) {
    St->Sum += ((ULONG)Data[I] << 8) | (ULONG)Data[I + 1];
  }

  if (I < Len) {
    St->Odd = TRUE;
    St->OddByte = Data[I];
  }
}

static __forceinline USHORT AerovNetCsumFold(_In_ ULONG Sum) {
  // Fold to 16 bits: add carries until none remain.
  while (Sum >> 16) {
    Sum = (Sum & 0xFFFFu) + (Sum >> 16);
  }
  return (USHORT)Sum;
}

static __forceinline USHORT AerovNetCsumFinalize(_In_ AEROVNET_CSUM_STATE* St) {
  ULONG Sum;

  if (!St) {
    return 0;
  }

  Sum = St->Sum;
  if (St->Odd) {
    Sum += ((ULONG)St->OddByte << 8);
  }

  return (USHORT)(~AerovNetCsumFold(Sum) & 0xFFFFu);
}

static __forceinline USHORT AerovNetCsumFoldState(_In_ const AEROVNET_CSUM_STATE* St) {
  ULONG Sum;

  if (!St) {
    return 0;
  }

  Sum = St->Sum;
  if (St->Odd) {
    Sum += ((ULONG)St->OddByte << 8);
  }
  return AerovNetCsumFold(Sum);
}

static BOOLEAN AerovNetNetBufferCopyBytes(_In_ PNET_BUFFER Nb, _In_ ULONG Offset, _Out_writes_bytes_(Bytes) UCHAR* Dest, _In_ ULONG Bytes) {
  PMDL Mdl;
  ULONG MdlOffset;
  ULONG Remaining;

  if (!Nb || !Dest) {
    return FALSE;
  }

  if (Bytes == 0) {
    return TRUE;
  }

  if (Offset + Bytes > NET_BUFFER_DATA_LENGTH(Nb)) {
    return FALSE;
  }

  Mdl = NET_BUFFER_CURRENT_MDL(Nb);
  MdlOffset = NET_BUFFER_CURRENT_MDL_OFFSET(Nb) + Offset;
  Remaining = Bytes;

  while (Mdl && Remaining) {
    ULONG MdlBytes;
    PUCHAR Va;
    ULONG Copy;

    MdlBytes = MmGetMdlByteCount(Mdl);
    if (MdlOffset >= MdlBytes) {
      MdlOffset -= MdlBytes;
      Mdl = Mdl->Next;
      continue;
    }

    Va = (PUCHAR)MmGetSystemAddressForMdlSafe(Mdl, NormalPagePriority);
    if (!Va) {
      return FALSE;
    }

    Copy = MdlBytes - MdlOffset;
    if (Copy > Remaining) {
      Copy = Remaining;
    }

    RtlCopyMemory(Dest, Va + MdlOffset, Copy);
    Dest += Copy;
    Remaining -= Copy;
    MdlOffset = 0;
    Mdl = Mdl->Next;
  }

  return (Remaining == 0);
}

static BOOLEAN AerovNetNetBufferWriteBytes(_In_ PNET_BUFFER Nb, _In_ ULONG Offset, _In_reads_bytes_(Bytes) const UCHAR* Src, _In_ ULONG Bytes) {
  PMDL Mdl;
  ULONG MdlOffset;
  ULONG Remaining;

  if (!Nb || !Src) {
    return FALSE;
  }

  if (Bytes == 0) {
    return TRUE;
  }

  if (Offset + Bytes > NET_BUFFER_DATA_LENGTH(Nb)) {
    return FALSE;
  }

  Mdl = NET_BUFFER_CURRENT_MDL(Nb);
  MdlOffset = NET_BUFFER_CURRENT_MDL_OFFSET(Nb) + Offset;
  Remaining = Bytes;

  while (Mdl && Remaining) {
    ULONG MdlBytes;
    PUCHAR Va;
    ULONG Copy;

    MdlBytes = MmGetMdlByteCount(Mdl);
    if (MdlOffset >= MdlBytes) {
      MdlOffset -= MdlBytes;
      Mdl = Mdl->Next;
      continue;
    }

    Va = (PUCHAR)MmGetSystemAddressForMdlSafe(Mdl, NormalPagePriority);
    if (!Va) {
      return FALSE;
    }

    Copy = MdlBytes - MdlOffset;
    if (Copy > Remaining) {
      Copy = Remaining;
    }

    RtlCopyMemory(Va + MdlOffset, Src, Copy);
    Src += Copy;
    Remaining -= Copy;
    MdlOffset = 0;
    Mdl = Mdl->Next;
  }

  return (Remaining == 0);
}

static VOID AerovNetCsumAccumulateNetBuffer(_Inout_ AEROVNET_CSUM_STATE* St, _In_ PNET_BUFFER Nb, _In_ ULONG Offset, _In_ ULONG Len) {
  PMDL Mdl;
  ULONG MdlOffset;
  ULONG Remaining;

  if (!St || !Nb || Len == 0) {
    return;
  }

  if (Offset + Len > NET_BUFFER_DATA_LENGTH(Nb)) {
    return;
  }

  Mdl = NET_BUFFER_CURRENT_MDL(Nb);
  MdlOffset = NET_BUFFER_CURRENT_MDL_OFFSET(Nb) + Offset;
  Remaining = Len;

  while (Mdl && Remaining) {
    ULONG MdlBytes;
    PUCHAR Va;
    ULONG Copy;

    MdlBytes = MmGetMdlByteCount(Mdl);
    if (MdlOffset >= MdlBytes) {
      MdlOffset -= MdlBytes;
      Mdl = Mdl->Next;
      continue;
    }

    Va = (PUCHAR)MmGetSystemAddressForMdlSafe(Mdl, NormalPagePriority);
    if (!Va) {
      return;
    }

    Copy = MdlBytes - MdlOffset;
    if (Copy > Remaining) {
      Copy = Remaining;
    }

    AerovNetCsumAccumulateBytes(St, Va + MdlOffset, Copy);
    Remaining -= Copy;
    MdlOffset = 0;
    Mdl = Mdl->Next;
  }
}

static VOID AerovNetCsumAccumulatePseudoHeader(_Inout_ AEROVNET_CSUM_STATE* St, _In_ const AEROVNET_PACKET_INFO* Info) {
  UCHAR Tmp[40];

  if (!St || !Info) {
    return;
  }

  RtlZeroMemory(Tmp, sizeof(Tmp));

  if (Info->L3 == AerovNetL3Ipv4) {
    // IPv4 pseudo header:
    //   src(4) dst(4) zero(1) proto(1) len(2)
    RtlCopyMemory(Tmp + 0, Info->SrcAddr, 4);
    RtlCopyMemory(Tmp + 4, Info->DstAddr, 4);
    Tmp[8] = 0;
    Tmp[9] = Info->IpProtocol;
    AerovNetWriteBe16(Tmp + 10, Info->L4Len);
    AerovNetCsumAccumulateBytes(St, Tmp, 12);
    return;
  }

  if (Info->L3 == AerovNetL3Ipv6) {
    // IPv6 pseudo header:
    //   src(16) dst(16) len(4) zero(3) next_header(1)
    RtlCopyMemory(Tmp + 0, Info->SrcAddr, 16);
    RtlCopyMemory(Tmp + 16, Info->DstAddr, 16);
    AerovNetWriteBe32(Tmp + 32, (ULONG)Info->L4Len);
    Tmp[39] = Info->IpProtocol;
    AerovNetCsumAccumulateBytes(St, Tmp, 40);
    return;
  }
}

static BOOLEAN AerovNetParsePacketInfo(_In_reads_bytes_(AvailLen) const UCHAR* Frame, _In_ ULONG FrameLen, _In_ ULONG AvailLen,
                                      _Out_ AEROVNET_PACKET_INFO* Info) {
  ULONG Offset;
  USHORT EtherType;
  ULONG L2Len;
  ULONG Tags;

  if (!Info) {
    return FALSE;
  }

  RtlZeroMemory(Info, sizeof(*Info));

  if (!Frame || FrameLen < 14 || AvailLen < 14) {
    return FALSE;
  }

  // Ethernet: dst(6) src(6) ethertype(2)
  EtherType = AerovNetReadBe16(Frame + 12);
  L2Len = 14;
  Tags = 0;

  while ((EtherType == AEROVNET_ETHERTYPE_VLAN || EtherType == AEROVNET_ETHERTYPE_QINQ || EtherType == AEROVNET_ETHERTYPE_VLAN_9100) && Tags < 2) {
    // VLAN tag: TPID(2) TCI(2) inner_ethertype(2)
    if (FrameLen < L2Len + 4) {
      return FALSE;
    }
    if (AvailLen < L2Len + 4) {
      return FALSE;
    }

    EtherType = AerovNetReadBe16(Frame + L2Len + 2);
    L2Len += 4;
    Tags++;
  }

  Info->L2Len = (USHORT)L2Len;
  Info->L3Offset = (USHORT)L2Len;

  if (EtherType == AEROVNET_ETHERTYPE_IPV4) {
    const ULONG IpOff = L2Len;
    UCHAR Vhl;
    ULONG Ihl;
    ULONG HdrLen;
    ULONG TotalLen;
    UCHAR Proto;
    USHORT Frag;
    ULONG L4Len;

    if (FrameLen < IpOff + 20 || AvailLen < IpOff + 20) {
      return FALSE;
    }

    Vhl = Frame[IpOff + 0];
    if ((Vhl >> 4) != 4) {
      return FALSE;
    }

    Ihl = (ULONG)(Vhl & 0x0F);
    HdrLen = Ihl * 4;
    if (HdrLen < 20 || (HdrLen & 3) != 0) {
      return FALSE;
    }
    if (FrameLen < IpOff + HdrLen || AvailLen < IpOff + HdrLen) {
      return FALSE;
    }

    TotalLen = (ULONG)AerovNetReadBe16(Frame + IpOff + 2);
    if (TotalLen < HdrLen) {
      return FALSE;
    }
    if (TotalLen > FrameLen - IpOff) {
      // Malformed length; clamp to the actual buffer to avoid OOB.
      TotalLen = FrameLen - IpOff;
    }

    Proto = Frame[IpOff + 9];
    Frag = AerovNetReadBe16(Frame + IpOff + 6);
    if ((Frag & 0x1FFFu) != 0 || (Frag & 0x2000u) != 0) {
      // Fragmented: do not attempt L4 parsing/offload (first fragment checksum covers whole packet).
      Info->L3 = AerovNetL3Ipv4;
      Info->Ipv4HeaderLen = (USHORT)HdrLen;
      Info->IpProtocol = Proto;
      RtlCopyMemory(Info->SrcAddr, Frame + IpOff + 12, 4);
      RtlCopyMemory(Info->DstAddr, Frame + IpOff + 16, 4);
      return TRUE;
    }

    Offset = IpOff + HdrLen;
    if (Offset > FrameLen) {
      return FALSE;
    }
    L4Len = TotalLen - HdrLen;
    if (L4Len > 0xFFFFu) {
      return FALSE;
    }

    Info->L3 = AerovNetL3Ipv4;
    Info->Ipv4HeaderLen = (USHORT)HdrLen;
    Info->IpProtocol = Proto;
    RtlCopyMemory(Info->SrcAddr, Frame + IpOff + 12, 4);
    RtlCopyMemory(Info->DstAddr, Frame + IpOff + 16, 4);

    if (Proto == 6) {
      // TCP.
      if (FrameLen < Offset + 20 || AvailLen < Offset + 20) {
        return FALSE;
      }
      Info->L4 = AerovNetL4Tcp;
      Info->L4Offset = (USHORT)Offset;
      Info->L4Len = (USHORT)L4Len;
      Info->L4CsumOffset = 16;
      return TRUE;
    }

    if (Proto == 17) {
      // UDP.
      if (FrameLen < Offset + 8 || AvailLen < Offset + 8) {
        return FALSE;
      }
      Info->L4 = AerovNetL4Udp;
      Info->L4Offset = (USHORT)Offset;
      Info->L4Len = (USHORT)L4Len;
      Info->L4CsumOffset = 6;
      return TRUE;
    }

    // IPv4 but unsupported L4 protocol.
    return TRUE;
  }

  if (EtherType == AEROVNET_ETHERTYPE_IPV6) {
    const ULONG IpOff = L2Len;
    UCHAR Ver;
    USHORT PayloadLen;
    UCHAR Next;
    ULONG Offset6;
    ULONG ExtLen;
    ULONG Iter;

    if (FrameLen < IpOff + 40 || AvailLen < IpOff + 40) {
      return FALSE;
    }

    Ver = (UCHAR)(Frame[IpOff + 0] >> 4);
    if (Ver != 6) {
      return FALSE;
    }

    PayloadLen = AerovNetReadBe16(Frame + IpOff + 4);
    Next = Frame[IpOff + 6];

    Info->L3 = AerovNetL3Ipv6;
    Info->Ipv4HeaderLen = 0;
    RtlCopyMemory(Info->SrcAddr, Frame + IpOff + 8, 16);
    RtlCopyMemory(Info->DstAddr, Frame + IpOff + 24, 16);

    Offset6 = IpOff + 40;
    ExtLen = 0;

    // Parse a limited set of extension headers to locate TCP/UDP.
    for (Iter = 0; Iter < 8; Iter++) {
      if (Next == 6 || Next == 17) {
        break;
      }

      if (Next == 0 || Next == 43 || Next == 60) {
        // Hop-by-Hop / Routing / Destination Options: next(1) hdrlen(1) ...
        ULONG HdrBytes;
        if (FrameLen < Offset6 + 8 || AvailLen < Offset6 + 8) {
          return FALSE;
        }
        HdrBytes = ((ULONG)Frame[Offset6 + 1] + 1u) * 8u;
        if (FrameLen < Offset6 + HdrBytes || AvailLen < Offset6 + HdrBytes) {
          return FALSE;
        }
        Next = Frame[Offset6 + 0];
        Offset6 += HdrBytes;
        ExtLen += HdrBytes;
        continue;
      }

      if (Next == 44) {
        // Fragment header: 8 bytes.
        USHORT Frag;
        if (FrameLen < Offset6 + 8 || AvailLen < Offset6 + 8) {
          return FALSE;
        }
        Next = Frame[Offset6 + 0];
        Frag = AerovNetReadBe16(Frame + Offset6 + 2);
        if ((Frag & 0xFFF8u) != 0 || (Frag & 0x0001u) != 0) {
          // Fragmented: do not attempt L4 parsing/offload.
          Info->IpProtocol = Next;
          return TRUE;
        }
        Offset6 += 8;
        ExtLen += 8;
        continue;
      }

      if (Next == 51) {
        // Authentication header: (Payload Len + 2) * 4 bytes.
        ULONG HdrBytes;
        if (FrameLen < Offset6 + 2 || AvailLen < Offset6 + 2) {
          return FALSE;
        }
        HdrBytes = ((ULONG)Frame[Offset6 + 1] + 2u) * 4u;
        if (FrameLen < Offset6 + HdrBytes || AvailLen < Offset6 + HdrBytes) {
          return FALSE;
        }
        Next = Frame[Offset6 + 0];
        Offset6 += HdrBytes;
        ExtLen += HdrBytes;
        continue;
      }

      // Unsupported extension header.
      Info->IpProtocol = Next;
      return TRUE;
    }

    if (PayloadLen < ExtLen) {
      return FALSE;
    }

    if (Next == 6) {
      ULONG L4Len;
      if (FrameLen < Offset6 + 20 || AvailLen < Offset6 + 20) {
        return FALSE;
      }
      L4Len = (ULONG)PayloadLen - ExtLen;
      if (L4Len > 0xFFFFu) {
        return FALSE;
      }
      Info->IpProtocol = 6;
      Info->L4 = AerovNetL4Tcp;
      Info->L4Offset = (USHORT)Offset6;
      Info->L4Len = (USHORT)L4Len;
      Info->L4CsumOffset = 16;
      return TRUE;
    }

    if (Next == 17) {
      ULONG L4Len;
      if (FrameLen < Offset6 + 8 || AvailLen < Offset6 + 8) {
        return FALSE;
      }
      L4Len = (ULONG)PayloadLen - ExtLen;
      if (L4Len > 0xFFFFu) {
        return FALSE;
      }
      Info->IpProtocol = 17;
      Info->L4 = AerovNetL4Udp;
      Info->L4Offset = (USHORT)Offset6;
      Info->L4Len = (USHORT)L4Len;
      Info->L4CsumOffset = 6;
      return TRUE;
    }

    // IPv6 but unsupported L4 protocol.
    Info->IpProtocol = Next;
    return TRUE;
  }

  return FALSE;
}

static BOOLEAN AerovNetComputeIpv4HeaderChecksum(_In_reads_bytes_(HdrLen) const UCHAR* Ipv4Hdr, _In_ ULONG HdrLen, _Out_ USHORT* OutChecksum) {
  AEROVNET_CSUM_STATE St;
  UCHAR Tmp[60];

  if (OutChecksum) {
    *OutChecksum = 0;
  }

  if (!Ipv4Hdr || !OutChecksum || HdrLen < 20 || HdrLen > sizeof(Tmp) || (HdrLen & 3) != 0) {
    return FALSE;
  }

  RtlCopyMemory(Tmp, Ipv4Hdr, HdrLen);
  Tmp[10] = 0;
  Tmp[11] = 0;

  RtlZeroMemory(&St, sizeof(St));
  AerovNetCsumAccumulateBytes(&St, Tmp, HdrLen);
  *OutChecksum = AerovNetCsumFinalize(&St);
  return TRUE;
}

static BOOLEAN AerovNetWriteNetBufferBe16(_In_ PNET_BUFFER Nb, _In_ ULONG Offset, _In_ USHORT V) {
  UCHAR Tmp[2];
  AerovNetWriteBe16(Tmp, V);
  return AerovNetNetBufferWriteBytes(Nb, Offset, Tmp, sizeof(Tmp));
}

static BOOLEAN AerovNetComputeAndWriteL4ChecksumNetBuffer(_In_ PNET_BUFFER Nb, _In_ const AEROVNET_PACKET_INFO* Info) {
  AEROVNET_CSUM_STATE St;
  ULONG L4Off;
  ULONG L4Len;
  ULONG CsumField;
  USHORT Csum;

  if (!Nb || !Info) {
    return FALSE;
  }

  if (Info->L4 != AerovNetL4Tcp && Info->L4 != AerovNetL4Udp) {
    return FALSE;
  }

  L4Off = Info->L4Offset;
  L4Len = Info->L4Len;
  CsumField = L4Off + (ULONG)Info->L4CsumOffset;

  if (L4Off + L4Len > NET_BUFFER_DATA_LENGTH(Nb)) {
    // Clamp to the actual NB length (Ethernet padding etc).
    L4Len = NET_BUFFER_DATA_LENGTH(Nb) - L4Off;
  }

  if (CsumField + 2 > L4Off + L4Len) {
    return FALSE;
  }

  RtlZeroMemory(&St, sizeof(St));
  AerovNetCsumAccumulatePseudoHeader(&St, Info);

  // L4 header+payload, with checksum field treated as zero.
  AerovNetCsumAccumulateNetBuffer(&St, Nb, L4Off, (ULONG)Info->L4CsumOffset);
  AerovNetCsumAccumulateNetBuffer(&St, Nb, CsumField + 2, (L4Off + L4Len) - (CsumField + 2));

  Csum = AerovNetCsumFinalize(&St);
  if (Info->L4 == AerovNetL4Udp && Csum == 0) {
    Csum = 0xFFFF;
  }

  return AerovNetWriteNetBufferBe16(Nb, CsumField, Csum);
}

static VOID AerovNetFreeTxRequestNoLock(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_TX_REQUEST* TxReq) {
  TxReq->State = AerovNetTxFree;
  TxReq->Cancelled = FALSE;
  TxReq->HeaderBuilt = FALSE;
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
  if (Pending <= 0) {
#if DBG
    DbgPrint("aero_virtio_net: tx: NBL pending underflow/double completion (pending=%ld)\n", Pending);
#endif
    return;
  }
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
  // Ensure TxReq completion is idempotent in case a cancellation/teardown path
  // races and attempts to complete the same request twice.
  TxReq->Nbl = NULL;
}

static __forceinline VOID AerovNetSgMappingsRefLocked(_Inout_ AEROVNET_ADAPTER* Adapter) {
  // Adapter->Lock must be held by the caller.
  if (Adapter->OutstandingSgMappings == 0) {
    KeClearEvent(&Adapter->OutstandingSgEvent);
  }
  Adapter->OutstandingSgMappings++;
}

static __forceinline VOID AerovNetSgMappingsDerefLocked(_Inout_ AEROVNET_ADAPTER* Adapter) {
  // Adapter->Lock must be held by the caller.
  if (Adapter->OutstandingSgMappings <= 0) {
#if DBG
    DbgPrint("aero_virtio_net: BUG: OutstandingSgMappings underflow (%ld)\n", Adapter->OutstandingSgMappings);
#endif
    Adapter->OutstandingSgMappings = 0;
    KeSetEvent(&Adapter->OutstandingSgEvent, IO_NO_INCREMENT, FALSE);
    return;
  }

  Adapter->OutstandingSgMappings--;
  if (Adapter->OutstandingSgMappings == 0) {
    KeSetEvent(&Adapter->OutstandingSgEvent, IO_NO_INCREMENT, FALSE);
  }
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

static BOOLEAN AerovNetIsValidIpv4HeaderChecksum(_In_reads_bytes_(IpHdrLen) const UCHAR* Ip, _In_ ULONG IpHdrLen) {
  ULONG Sum;
  ULONG I;

  if (!Ip || IpHdrLen < 20u || (IpHdrLen & 1u) != 0u) {
    return FALSE;
  }

  Sum = 0;
  for (I = 0; I < IpHdrLen; I += 2u) {
    Sum += ((ULONG)Ip[I] << 8) | (ULONG)Ip[I + 1u];
  }

  while ((Sum >> 16) != 0) {
    Sum = (Sum & 0xFFFFu) + (Sum >> 16);
  }

  // For a valid IPv4 header checksum, the one's-complement sum of all 16-bit
  // words (including the checksum field) is 0xFFFF.
  return ((USHORT)Sum == 0xFFFFu) ? TRUE : FALSE;
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

static VOID AerovNetIndicateRxChecksum(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PNET_BUFFER_LIST Nbl,
                                       _In_reads_bytes_(FrameLen) const UCHAR* Frame, _In_ ULONG FrameLen, _In_ const VIRTIO_NET_HDR* Vhdr) {
  NDIS_TCP_IP_CHECKSUM_NET_BUFFER_LIST_INFO CsumInfo;
  VIRTIO_NET_HDR_OFFLOAD_RX_INFO RxInfo;
  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO FrameInfo;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;

  if (!Adapter || !Nbl) {
    return;
  }

  // NBLs are recycled; always clear the checksum indication to avoid leaking
  // status between frames.
  NET_BUFFER_LIST_INFO(Nbl, TcpIpChecksumNetBufferListInfo) = NULL;

  // Only trust RX checksum metadata when the device negotiated guest checksum
  // support (VIRTIO_NET_F_GUEST_CSUM). VIRTIO_NET_F_CSUM covers TX checksum
  // offload only.
  if ((Adapter->GuestFeatures & VIRTIO_NET_F_GUEST_CSUM) == 0) {
    return;
  }
  if (!Vhdr) {
    return;
  }

  (void)VirtioNetHdrOffloadParseRxHdr(Vhdr, &RxInfo);

  // If the device requests that the guest compute a checksum, complete it in
  // software to avoid indicating a packet with an invalid checksum up the
  // stack. (Virtio's NEEDS_CSUM scheme is a "partial checksum" completion.)
  if (RxInfo.NeedsCsum) {
    PNET_BUFFER Nb;
    AEROVNET_PACKET_INFO Pkt;

    Nb = NET_BUFFER_LIST_FIRST_NB(Nbl);
    if (Nb && AerovNetParsePacketInfo(Frame, FrameLen, FrameLen, &Pkt)) {
      (VOID)AerovNetComputeAndWriteL4ChecksumNetBuffer(Nb, &Pkt);
    }
    return;
  }

  // Only indicate checksum success when the device explicitly marks the data as
  // validated.
  if (!RxInfo.CsumValid) {
    return;
  }

  if (!Frame || FrameLen < 14) {
    return;
  }

  St = VirtioNetHdrOffloadParseFrame((const uint8_t*)Frame, (size_t)FrameLen, &FrameInfo);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return;
  }

  CsumInfo.Value = 0;

  // Do not claim L4 checksum validity on fragmented packets: the checksum covers
  // the reassembled payload, which this miniport does not validate.
  if (!FrameInfo.IsFragmented) {
    BOOLEAN RxEnabled = FALSE;
    BOOLEAN IpHdrChecked = FALSE;
    BOOLEAN IpHdrValid = FALSE;

    if (FrameInfo.L3Proto == (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV4) {
      // IPv4 includes a header checksum. virtio-net's DATA_VALID flag indicates
      // L4 checksum validity; validate the IPv4 header checksum directly to avoid
      // claiming success without verification.
      {
        const ULONG IpOffset = (ULONG)FrameInfo.L3Offset;
        const ULONG IpHdrLen = (ULONG)FrameInfo.L3Len;
        IpHdrChecked = TRUE;
        IpHdrValid = (IpOffset + IpHdrLen <= FrameLen) ? AerovNetIsValidIpv4HeaderChecksum(Frame + IpOffset, IpHdrLen) : FALSE;
      }

      if (FrameInfo.L4Proto == 6u) {
        RxEnabled = Adapter->RxChecksumV4Enabled;
        if (RxEnabled) {
          if (IpHdrChecked) {
            if (IpHdrValid) {
              CsumInfo.Receive.IpChecksumSucceeded = 1;
            } else {
              CsumInfo.Receive.IpChecksumFailed = 1;
            }
          }
          CsumInfo.Receive.TcpChecksumSucceeded = 1;
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatRxCsumValidatedTcp4);
        }
      } else if (FrameInfo.L4Proto == 17u) {
        RxEnabled = Adapter->RxUdpChecksumV4Enabled;
        if (RxEnabled) {
          if (IpHdrChecked) {
            if (IpHdrValid) {
              CsumInfo.Receive.IpChecksumSucceeded = 1;
            } else {
              CsumInfo.Receive.IpChecksumFailed = 1;
            }
          }
          CsumInfo.Receive.UdpChecksumSucceeded = 1;
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatRxCsumValidatedUdp4);
        }
      }
    } else if (FrameInfo.L3Proto == (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV6) {
      if (FrameInfo.L4Proto == 6u) {
        RxEnabled = Adapter->RxChecksumV6Enabled;
        if (RxEnabled) {
          CsumInfo.Receive.TcpChecksumSucceeded = 1;
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatRxCsumValidatedTcp6);
        }
      } else if (FrameInfo.L4Proto == 17u) {
        RxEnabled = Adapter->RxUdpChecksumV6Enabled;
        if (RxEnabled) {
          CsumInfo.Receive.UdpChecksumSucceeded = 1;
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatRxCsumValidatedUdp6);
        }
      }
    }
  }

  if (CsumInfo.Value != 0) {
    NET_BUFFER_LIST_INFO(Nbl, TcpIpChecksumNetBufferListInfo) = (PVOID)(ULONG_PTR)CsumInfo.Value;
  }
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
  USHORT MsgDescCount;
  USHORT MsgCountMax;

  Adapter->Bar0Va = NULL;
  Adapter->Bar0Length = 0;
  Adapter->Bar0Pa.QuadPart = 0;
  RtlZeroMemory(&Adapter->Vdev, sizeof(Adapter->Vdev));
  Adapter->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
  Adapter->MsixRxVector = VIRTIO_PCI_MSI_NO_VECTOR;
  Adapter->MsixTxVector = VIRTIO_PCI_MSI_NO_VECTOR;
  Adapter->UseMsix = FALSE;
  Adapter->MsixAllOnVector0 = FALSE;
  Adapter->MsixMessageCount = 0;

  if (!Resources) {
    return NDIS_STATUS_RESOURCES;
  }

  // Interrupt resources (MSI/MSI-X vs INTx).
  //
  // If Windows allocated message-signaled interrupts, the translated resource list contains a
  // CmResourceTypeInterrupt descriptor with CM_RESOURCE_INTERRUPT_MESSAGE set and a MessageCount
  // field. Prefer MSI/MSI-X when present; INTx remains the fallback.
  MsgDescCount = 0;
  MsgCountMax = 0;
  for (I = 0; I < Resources->Count; I++) {
    const CM_PARTIAL_RESOURCE_DESCRIPTOR* Desc = &Resources->PartialDescriptors[I];
    USHORT MessageCount;

    if (Desc->Type != CmResourceTypeInterrupt) {
      continue;
    }

    if ((Desc->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) == 0) {
      continue;
    }

    MsgDescCount++;
    MessageCount = Desc->u.MessageInterrupt.MessageCount;
    if (MessageCount > MsgCountMax) {
      MsgCountMax = MessageCount;
    }
  }

  // Windows typically reports message interrupts as a single descriptor with a MessageCount,
  // but some stacks may represent them as multiple descriptors. Prefer the largest explicit
  // MessageCount value, but fall back to counting message interrupt descriptors so we don't
  // accidentally under-detect available messages.
  {
    USHORT MessageCount = MsgCountMax;
    if (MsgDescCount > MessageCount) {
      MessageCount = MsgDescCount;
    }

    if (MessageCount != 0) {
      Adapter->UseMsix = TRUE;
      Adapter->MsixMessageCount = MessageCount;

      // virtio-net benefits from at least 3 vectors (config + RX + TX). If Windows granted fewer,
      // route all interrupts to vector 0.
      Adapter->MsixConfigVector = 0;
      if (MessageCount >= 3u) {
        Adapter->MsixAllOnVector0 = FALSE;
        Adapter->MsixRxVector = 1;
        Adapter->MsixTxVector = 2;
      } else {
        Adapter->MsixAllOnVector0 = TRUE;
        Adapter->MsixRxVector = 0;
        Adapter->MsixTxVector = 0;
      }
    }
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

  /* BAR0 layout validation (strict vs permissive is controlled at build time by AERO_VIRTIO_MINIPORT_ENFORCE_FIXED_LAYOUT). */
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
    Rx->Mdl->Next = NULL;
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

static VOID AerovNetResetRxBufferForReuse(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_RX_BUFFER* Rx) {
  if (!Adapter || !Rx) {
    return;
  }

  Rx->Indicated = FALSE;
  Rx->PacketNext = NULL;
  Rx->PacketBytes = 0;

  if (Rx->Nbl) {
    NET_BUFFER_LIST_INFO(Rx->Nbl, TcpIpChecksumNetBufferListInfo) = NULL;
  }

  if (Rx->Mdl) {
    Rx->Mdl->Next = NULL;
    Rx->Mdl->ByteCount = Adapter->RxBufferDataBytes;
  }

  if (Rx->Nb) {
    // Ensure the NET_BUFFER points at the payload MDL with a clean offset/length.
    NET_BUFFER_CURRENT_MDL(Rx->Nb) = Rx->Mdl;
    NET_BUFFER_CURRENT_MDL_OFFSET(Rx->Nb) = 0;
    NET_BUFFER_DATA_OFFSET(Rx->Nb) = 0;
    NET_BUFFER_DATA_LENGTH(Rx->Nb) = 0;
  }
}

static VOID AerovNetRecycleRxPacketLocked(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_RX_BUFFER* RxHead) {
  AEROVNET_RX_BUFFER* Rx;

  if (!Adapter || !RxHead) {
    return;
  }

  Rx = RxHead;
  while (Rx) {
    AEROVNET_RX_BUFFER* Next = (AEROVNET_RX_BUFFER*)Rx->PacketNext;
    AerovNetResetRxBufferForReuse(Adapter, Rx);
    InsertTailList(&Adapter->RxFreeList, &Rx->Link);
    Rx = Next;
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

  if (Adapter->RxChecksumScratch) {
    ExFreePoolWithTag(Adapter->RxChecksumScratch, AEROVNET_TAG);
    Adapter->RxChecksumScratch = NULL;
    Adapter->RxChecksumScratchBytes = 0;
  }

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

  // Ensure no synchronous ctrl_vq command is still running before we tear down
  // the virtio queues and free any pending control buffers.
  if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
    (VOID)KeWaitForSingleObject(&Adapter->CtrlCmdEvent, Executive, KernelMode, FALSE, NULL);
  }

  // Device is already stopped/reset by the caller.
  AerovNetFreeTxResources(Adapter);
  AerovNetFreeRxResources(Adapter);
  AerovNetFreeCtrlPendingRequests(Adapter);

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
  AerovNetFreeVq(Adapter, &Adapter->CtrlVq);

  if (Adapter->CtrlVqRegKey) {
    ZwClose(Adapter->CtrlVqRegKey);
    Adapter->CtrlVqRegKey = NULL;
  }

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
    ULONG RxHdrBytes;

    // Each receive buffer is posted as a header + payload descriptor chain.
    if (Adapter->RxVq.QueueSize == 0) {
      break;
    }

    Entry = RemoveHeadList(&Adapter->RxFreeList);
    Rx = CONTAINING_RECORD(Entry, AEROVNET_RX_BUFFER, Link);

    Rx->Indicated = FALSE;
    Rx->PacketNext = NULL;
    Rx->PacketBytes = 0;

    RxHdrBytes = Adapter->RxHeaderBytes;
    // Ensure the virtio-net header doesn't retain stale data if the device
    // chooses not to write some header fields for a particular packet.
    RtlZeroMemory(Rx->BufferVa, RxHdrBytes);

    Sg[0].addr = (uint64_t)Rx->BufferPa.QuadPart;
    Sg[0].len = (uint32_t)RxHdrBytes;
    Sg[0].device_writes = VIRTIO_TRUE;

    Sg[1].addr = (uint64_t)Rx->BufferPa.QuadPart + (uint64_t)RxHdrBytes;
    Sg[1].len = (uint32_t)(Rx->BufferBytes - RxHdrBytes);
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

static ULONG AerovNetChecksumAdd(_In_ ULONG Sum, _In_reads_bytes_(Len) const UCHAR* Buf, _In_ ULONG Len) {
  ULONG I;

  if (!Buf) {
    return Sum;
  }

  for (I = 0; I + 1 < Len; I += 2) {
    Sum += ((ULONG)Buf[I] << 8) | (ULONG)Buf[I + 1];
  }

  if ((Len & 1u) != 0) {
    Sum += (ULONG)Buf[Len - 1] << 8;
  }

  return Sum;
}

static USHORT AerovNetChecksumFinish(_In_ ULONG Sum) {
  while ((Sum >> 16) != 0) {
    Sum = (Sum & 0xFFFFu) + (Sum >> 16);
  }

  {
    USHORT Csum = (USHORT)~(USHORT)Sum;
    // RFC 768/793: if the computed checksum is 0, transmit it as all-ones.
    return (Csum == 0) ? (USHORT)0xFFFFu : Csum;
  }
}

static BOOLEAN AerovNetWriteNetBufferData(_Inout_ PNET_BUFFER Nb, _In_ ULONG Offset, _In_reads_bytes_(Len) const UCHAR* Data, _In_ ULONG Len) {
  PMDL Mdl;
  ULONG MdlOffset;
  ULONG Skip;

  if (!Nb || !Data) {
    return FALSE;
  }

  if (Offset + Len > NET_BUFFER_DATA_LENGTH(Nb)) {
    return FALSE;
  }

  Mdl = NET_BUFFER_CURRENT_MDL(Nb);
  MdlOffset = NET_BUFFER_CURRENT_MDL_OFFSET(Nb);
  Skip = Offset;

  while (Mdl) {
    ULONG ByteCount;
    ULONG Available;

    ByteCount = MmGetMdlByteCount(Mdl);
    if (ByteCount < MdlOffset) {
      return FALSE;
    }

    Available = ByteCount - MdlOffset;
    if (Skip < Available) {
      break;
    }

    Skip -= Available;
    Mdl = NDIS_MDL_LINKAGE(Mdl);
    MdlOffset = 0;
  }

  if (!Mdl) {
    return FALSE;
  }

  MdlOffset += Skip;

  while (Len != 0 && Mdl) {
    ULONG ByteCount;
    PUCHAR Va;
    ULONG Available;
    ULONG ToCopy;

    ByteCount = MmGetMdlByteCount(Mdl);
    if (ByteCount < MdlOffset) {
      return FALSE;
    }

    Va = (PUCHAR)MmGetSystemAddressForMdlSafe(Mdl, NormalPagePriority);
    if (!Va) {
      return FALSE;
    }

    Available = ByteCount - MdlOffset;
    ToCopy = (Len < Available) ? Len : Available;
    RtlCopyMemory(Va + MdlOffset, Data, ToCopy);

    Data += ToCopy;
    Len -= ToCopy;

    Mdl = NDIS_MDL_LINKAGE(Mdl);
    MdlOffset = 0;
  }

  return (Len == 0) ? TRUE : FALSE;
}

static BOOLEAN AerovNetComputeAndWriteL4Checksum(_Inout_ PNET_BUFFER Nb,
                                                _In_reads_bytes_(FrameLen) const UCHAR* Frame,
                                                _In_ ULONG FrameLen,
                                                _In_ UCHAR ExpectedL4Proto) {
  VIRTIO_NET_HDR_OFFLOAD_FRAME_INFO Info;
  VIRTIO_NET_HDR_OFFLOAD_STATUS St;
  ULONG L4TotalLen;
  ULONG Sum;
  ULONG CsumAbsOffset;
  USHORT Csum;

  if (!Nb || !Frame || FrameLen < 14) {
    return FALSE;
  }

  RtlZeroMemory(&Info, sizeof(Info));
  St = VirtioNetHdrOffloadParseFrame((const uint8_t*)Frame, (size_t)FrameLen, &Info);
  if (St != VIRTIO_NET_HDR_OFFLOAD_STATUS_OK) {
    return FALSE;
  }

  if (Info.IsFragmented) {
    // Transport checksum offload doesn't apply to fragmented packets; assume the
    // stack already produced a correct checksum.
    return TRUE;
  }

  if (Info.L4Proto != ExpectedL4Proto) {
    return FALSE;
  }

  if (Info.L3Proto == (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV4) {
    const UCHAR* Ip;
    ULONG TotalLen;

    if (FrameLen < (ULONG)Info.L3Offset + 20u) {
      return FALSE;
    }

    Ip = Frame + Info.L3Offset;
    TotalLen = AerovNetReadBe16(Ip + 2);
    if (TotalLen < (ULONG)Info.L3Len) {
      return FALSE;
    }

    L4TotalLen = TotalLen - (ULONG)Info.L3Len;
  } else if (Info.L3Proto == (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV6) {
    const UCHAR* Ip6;
    ULONG PayloadLen;
    ULONG ExtLen;

    if (FrameLen < (ULONG)Info.L3Offset + 40u) {
      return FALSE;
    }

    Ip6 = Frame + Info.L3Offset;
    PayloadLen = AerovNetReadBe16(Ip6 + 4);

    if (Info.L3Len < 40u) {
      return FALSE;
    }

    ExtLen = (ULONG)Info.L3Len - 40u;
    if (PayloadLen < ExtLen) {
      return FALSE;
    }

    L4TotalLen = PayloadLen - ExtLen;
  } else {
    return FALSE;
  }

  if (FrameLen < (ULONG)Info.L4Offset + L4TotalLen) {
    return FALSE;
  }

  // Build pseudo header checksum.
  Sum = 0;

  if (Info.L3Proto == (uint8_t)VIRTIO_NET_HDR_OFFLOAD_L3_IPV4) {
    const UCHAR* Ip = Frame + Info.L3Offset;
    // IPv4 pseudo header: src(4) + dst(4) + zero+proto(2) + length(2).
    Sum = AerovNetChecksumAdd(Sum, Ip + 12, 8);
    Sum += (USHORT)Info.L4Proto;
    Sum += (USHORT)L4TotalLen;
  } else {
    const UCHAR* Ip6 = Frame + Info.L3Offset;
    // IPv6 pseudo header: src(16) + dst(16) + length(4) + zero(3) + next(1).
    Sum = AerovNetChecksumAdd(Sum, Ip6 + 8, 32);
    Sum += (USHORT)((L4TotalLen >> 16) & 0xFFFFu);
    Sum += (USHORT)(L4TotalLen & 0xFFFFu);
    Sum += (USHORT)Info.L4Proto;
  }

  // Add L4 header+payload, with the checksum field treated as zero.
  CsumAbsOffset = (ULONG)Info.CsumStart + (ULONG)Info.CsumOffset;

  if (CsumAbsOffset + 1u >= FrameLen) {
    return FALSE;
  }

  {
    const UCHAR* L4 = Frame + Info.L4Offset;
    ULONG I;
    ULONG Abs;

    // Word-wise sum over the L4 region (big-endian 16-bit words).
    Abs = (ULONG)Info.L4Offset;
    for (I = 0; I + 1 < L4TotalLen; I += 2) {
      UCHAR B0 = L4[I];
      UCHAR B1 = L4[I + 1];

      if (Abs == CsumAbsOffset || Abs == CsumAbsOffset + 1u) {
        B0 = 0;
      }
      if (Abs + 1u == CsumAbsOffset || Abs + 1u == CsumAbsOffset + 1u) {
        B1 = 0;
      }

      Sum += ((ULONG)B0 << 8) | (ULONG)B1;
      Abs += 2;
    }

    if ((L4TotalLen & 1u) != 0) {
      UCHAR B = L4[L4TotalLen - 1];
      if (Abs == CsumAbsOffset || Abs == CsumAbsOffset + 1u) {
        B = 0;
      }
      Sum += (ULONG)B << 8;
    }
  }

  Csum = AerovNetChecksumFinish(Sum);

  {
    UCHAR Bytes[2];
    Bytes[0] = (UCHAR)(Csum >> 8);
    Bytes[1] = (UCHAR)(Csum & 0xFF);
    return AerovNetWriteNetBufferData(Nb, CsumAbsOffset, Bytes, sizeof(Bytes));
  }
}

static NDIS_STATUS AerovNetBuildTxHeader(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_TX_REQUEST* TxReq) {
  AEROVNET_VIRTIO_NET_HDR BuiltHdr;
  AEROVNET_TX_OFFLOAD_INTENT Intent;
  AEROVNET_OFFLOAD_PARSE_INFO Info;
  NDIS_TCP_IP_CHECKSUM_NET_BUFFER_LIST_INFO CsumInfo;
  ULONG FrameLen;
  UCHAR HeaderBytes[256];
  UCHAR FullFrameBytes[2048];
  ULONG CopyLen;
  PVOID FramePtr;
  AEROVNET_OFFLOAD_RESULT OffRes;
  BOOLEAN WantIpHdrChecksum;
  BOOLEAN WantTcpChecksum;
  BOOLEAN WantUdpChecksum;
  BOOLEAN WantL4Checksum;

  if (!Adapter || !TxReq || !TxReq->Nbl || !TxReq->Nb || !TxReq->HeaderVa) {
    return NDIS_STATUS_INVALID_PACKET;
  }

  // Contract v1 behavior for non-offload packets: virtio-net header is all zeros.
  RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);

  RtlZeroMemory(&Intent, sizeof(Intent));
  RtlZeroMemory(&BuiltHdr, sizeof(BuiltHdr));
  RtlZeroMemory(&Info, sizeof(Info));

  WantIpHdrChecksum = FALSE;
  WantTcpChecksum = FALSE;
  WantUdpChecksum = FALSE;
  WantL4Checksum = FALSE;

  // LSO/TSO request (per-NBL).
  {
    ULONG_PTR LsoVal = (ULONG_PTR)NET_BUFFER_LIST_INFO(TxReq->Nbl, TcpLargeSendNetBufferListInfo);
    if (LsoVal != 0) {
      USHORT Mss = (USHORT)(LsoVal & 0xFFFFFu); // MSS is stored in the low 20 bits.
      Intent.WantTso = 1;
      Intent.TsoMss = Mss;
      // Enable virtio-net ECN semantics for TSO packets when supported by the host.
      Intent.TsoEcn = (Adapter->GuestFeatures & VIRTIO_NET_F_HOST_ECN) ? 1u : 0u;
    }
  }

  // Non-TSO packets rely on NDIS checksum metadata.
  if (!Intent.WantTso) {
    CsumInfo.Value = (ULONG_PTR)NET_BUFFER_LIST_INFO(TxReq->Nbl, TcpIpChecksumNetBufferListInfo);
    WantTcpChecksum = CsumInfo.Transmit.TcpChecksum ? TRUE : FALSE;
    WantUdpChecksum = CsumInfo.Transmit.UdpChecksum ? TRUE : FALSE;
    WantIpHdrChecksum = CsumInfo.Transmit.IpHeaderChecksum ? TRUE : FALSE;
    WantL4Checksum = (WantTcpChecksum || WantUdpChecksum) ? TRUE : FALSE;

    Intent.WantTcpChecksum = WantTcpChecksum ? 1u : 0u;
    Intent.WantUdpChecksum = WantUdpChecksum ? 1u : 0u;

    if (!WantIpHdrChecksum && !WantL4Checksum) {
      // Normal packet: all zeros.
      return NDIS_STATUS_SUCCESS;
    }
  } else {
    // TSO implies checksum offload; ensure the device negotiated checksum support.
    if (!Adapter->TxChecksumSupported) {
      return NDIS_STATUS_INVALID_PACKET;
    }
  }

  FrameLen = NET_BUFFER_DATA_LENGTH(TxReq->Nb);
  // Copy the start of the frame into a contiguous buffer so header parsing is
  // robust even when the NET_BUFFER spans multiple MDLs.
  //
  // - Checksum-only packets are always small (<= 1522 bytes): copy the full
  //   frame so checksum fallback can access the whole packet.
  // - TSO packets can be large: start with a small copy window and retry with a
  //   larger one if header parsing indicates truncation (e.g. long IPv6
  //   extension header chains).
  if (Intent.WantTso) {
    CopyLen = (FrameLen < sizeof(HeaderBytes)) ? FrameLen : (ULONG)sizeof(HeaderBytes);
    FramePtr = NdisGetDataBuffer(TxReq->Nb, CopyLen, HeaderBytes, 1, 0);
  } else {
    CopyLen = (FrameLen < sizeof(FullFrameBytes)) ? FrameLen : (ULONG)sizeof(FullFrameBytes);
    FramePtr = NdisGetDataBuffer(TxReq->Nb, CopyLen, FullFrameBytes, 1, 0);
  }
  if (!FramePtr) {
    return NDIS_STATUS_INVALID_PACKET;
  }

  // Best-effort: if the OS requested IPv4 header checksum offload, compute it in software.
  if (WantIpHdrChecksum) {
    AEROVNET_PACKET_INFO Pkt;
    USHORT Ipv4HdrCsum;
    RtlZeroMemory(&Pkt, sizeof(Pkt));
    Ipv4HdrCsum = 0;
    if (AerovNetParsePacketInfo((const UCHAR*)FramePtr, FrameLen, CopyLen, &Pkt)) {
      if (Pkt.L3 == AerovNetL3Ipv4 && Pkt.Ipv4HeaderLen != 0) {
        const ULONG IpOff = (ULONG)Pkt.L3Offset;
        if (CopyLen >= IpOff + (ULONG)Pkt.Ipv4HeaderLen) {
          if (AerovNetComputeIpv4HeaderChecksum((const UCHAR*)FramePtr + IpOff, (ULONG)Pkt.Ipv4HeaderLen, &Ipv4HdrCsum)) {
            (VOID)AerovNetWriteNetBufferBe16(TxReq->Nb, IpOff + 10, Ipv4HdrCsum);
          }
        }
      }
    }
  }

  // If only IPv4 header checksum was requested, we're done (virtio header stays zero).
  if (!Intent.WantTso && !WantL4Checksum) {
    return NDIS_STATUS_SUCCESS;
  }

  OffRes = AerovNetBuildTxVirtioNetHdr((const uint8_t*)FramePtr, (size_t)CopyLen, &Intent, &BuiltHdr, &Info);
  if (Intent.WantTso && OffRes == AEROVNET_OFFLOAD_ERR_FRAME_TOO_SHORT && CopyLen < FrameLen) {
    /*
     * Large TSO frames may have uncommon-but-valid header layouts (e.g. long IPv6
     * extension header chains) that exceed our small header buffer. Retry with a
     * larger copy window (still bounded) before rejecting.
     */
    ULONG RetryLen = (FrameLen < sizeof(FullFrameBytes)) ? FrameLen : (ULONG)sizeof(FullFrameBytes);
    if (RetryLen > CopyLen) {
      CopyLen = RetryLen;
      FramePtr = NdisGetDataBuffer(TxReq->Nb, CopyLen, FullFrameBytes, 1, 0);
      if (!FramePtr) {
        return NDIS_STATUS_INVALID_PACKET;
      }
      OffRes = AerovNetBuildTxVirtioNetHdr((const uint8_t*)FramePtr, (size_t)CopyLen, &Intent, &BuiltHdr, &Info);
    }
  }
  if (OffRes != AEROVNET_OFFLOAD_OK) {
    // TSO cannot be emulated in software at this layer; reject.
    if (Intent.WantTso) {
      return NDIS_STATUS_INVALID_PACKET;
    }

    // For checksum-only requests, fall back to software checksumming when
    // possible (or send with no offload metadata for non-applicable frames).
    if (Intent.WantTcpChecksum) {
      InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
      if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 6u)) {
        return NDIS_STATUS_INVALID_PACKET;
      }
#if DBG
      InterlockedIncrement(&g_AerovNetDbgTxTcpCsumFallback);
#endif
      Adapter->StatTxTcpCsumFallback++;
    } else if (Intent.WantUdpChecksum) {
      InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
      if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 17u)) {
        return NDIS_STATUS_INVALID_PACKET;
      }
#if DBG
      InterlockedIncrement(&g_AerovNetDbgTxUdpCsumFallback);
#endif
      Adapter->StatTxUdpCsumFallback++;
    } else {
      return NDIS_STATUS_INVALID_PACKET;
    }

    RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);
    return NDIS_STATUS_SUCCESS;
  }

  // Validate negotiated capabilities and the offload enablement that was in effect
  // when this request was accepted. Offload enablement can change at runtime via
  // OID_TCP_OFFLOAD_PARAMETERS, so queued/pending sends must not consult the live
  // adapter config.
  if (Intent.WantTso) {
    if (Intent.TsoMss == 0) {
      return NDIS_STATUS_INVALID_PACKET;
    }
    if (Info.IpVersion == 4) {
      if (!Adapter->TxTsoV4Supported || !TxReq->TxTsoV4Enabled || !TxReq->TxChecksumV4Enabled) {
        return NDIS_STATUS_INVALID_PACKET;
      }
    } else if (Info.IpVersion == 6) {
      if (!Adapter->TxTsoV6Supported || !TxReq->TxTsoV6Enabled || !TxReq->TxChecksumV6Enabled) {
        return NDIS_STATUS_INVALID_PACKET;
      }
    } else {
      return NDIS_STATUS_INVALID_PACKET;
    }
  } else {
    // Checksum offload only.
    if (!Adapter->TxChecksumSupported) {
      // Host doesn't support checksum offload; compute in software.
      if (Intent.WantTcpChecksum) {
        InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
        if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 6u)) {
          return NDIS_STATUS_INVALID_PACKET;
        }
#if DBG
        InterlockedIncrement(&g_AerovNetDbgTxTcpCsumFallback);
#endif
        Adapter->StatTxTcpCsumFallback++;
      } else if (Intent.WantUdpChecksum) {
        InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
        if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 17u)) {
          return NDIS_STATUS_INVALID_PACKET;
        }
#if DBG
        InterlockedIncrement(&g_AerovNetDbgTxUdpCsumFallback);
#endif
        Adapter->StatTxUdpCsumFallback++;
      }

      RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);
      return NDIS_STATUS_SUCCESS;
    }

    if (Intent.WantTcpChecksum && Intent.WantUdpChecksum) {
      return NDIS_STATUS_INVALID_PACKET;
    }

    if (Info.IpVersion == 4) {
      if (Intent.WantTcpChecksum) {
        if (!TxReq->TxChecksumV4Enabled) {
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
          if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 6u)) {
            return NDIS_STATUS_INVALID_PACKET;
          }
#if DBG
          InterlockedIncrement(&g_AerovNetDbgTxTcpCsumFallback);
#endif
          Adapter->StatTxTcpCsumFallback++;
          RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);
          return NDIS_STATUS_SUCCESS;
        }
      } else if (Intent.WantUdpChecksum) {
        if (!TxReq->TxUdpChecksumV4Enabled) {
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
          if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 17u)) {
            return NDIS_STATUS_INVALID_PACKET;
          }
#if DBG
          InterlockedIncrement(&g_AerovNetDbgTxUdpCsumFallback);
#endif
          Adapter->StatTxUdpCsumFallback++;
          RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);
          return NDIS_STATUS_SUCCESS;
        }
      } else {
        return NDIS_STATUS_INVALID_PACKET;
      }
    } else if (Info.IpVersion == 6) {
      if (Intent.WantTcpChecksum) {
        if (!TxReq->TxChecksumV6Enabled) {
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
          if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 6u)) {
            return NDIS_STATUS_INVALID_PACKET;
          }
#if DBG
          InterlockedIncrement(&g_AerovNetDbgTxTcpCsumFallback);
#endif
          Adapter->StatTxTcpCsumFallback++;
          RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);
          return NDIS_STATUS_SUCCESS;
        }
      } else if (Intent.WantUdpChecksum) {
        if (!TxReq->TxUdpChecksumV6Enabled) {
          InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumFallback);
          if (!AerovNetComputeAndWriteL4Checksum(TxReq->Nb, (const UCHAR*)FramePtr, CopyLen, 17u)) {
            return NDIS_STATUS_INVALID_PACKET;
          }
#if DBG
          InterlockedIncrement(&g_AerovNetDbgTxUdpCsumFallback);
#endif
          Adapter->StatTxUdpCsumFallback++;
          RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);
          return NDIS_STATUS_SUCCESS;
        }
      } else {
        return NDIS_STATUS_INVALID_PACKET;
      }
    } else {
      return NDIS_STATUS_INVALID_PACKET;
    }
  }

  if (Intent.WantUdpChecksum) {
    Adapter->StatTxUdpCsumOffload++;
  } else if (Intent.WantTcpChecksum || Intent.WantTso) {
    Adapter->StatTxTcpCsumOffload++;
  }

  // For checksum offload, virtio-net expects the checksum field in the packet to
  // contain the pseudo-header checksum. Compute and write it.
  if (BuiltHdr.Flags & AEROVNET_VIRTIO_NET_HDR_F_NEEDS_CSUM) {
    AEROVNET_PACKET_INFO Pkt;
    AEROVNET_CSUM_STATE Pseudo;
    USHORT PseudoSum;
    ULONG CsumFieldOffset;

    RtlZeroMemory(&Pkt, sizeof(Pkt));
    RtlZeroMemory(&Pseudo, sizeof(Pseudo));
    PseudoSum = 0;

    if (AerovNetParsePacketInfo((const UCHAR*)FramePtr, FrameLen, CopyLen, &Pkt)) {
      AerovNetCsumAccumulatePseudoHeader(&Pseudo, &Pkt);
      PseudoSum = AerovNetCsumFoldState(&Pseudo);

      CsumFieldOffset = (ULONG)BuiltHdr.CsumStart + (ULONG)BuiltHdr.CsumOffset;
      if (CsumFieldOffset + 2 <= FrameLen) {
        (VOID)AerovNetWriteNetBufferBe16(TxReq->Nb, CsumFieldOffset, PseudoSum);
      }
    }
  }

#if DBG
  if (Intent.WantUdpChecksum) {
    InterlockedIncrement(&g_AerovNetDbgTxUdpCsumOffload);
  } else if (Intent.WantTcpChecksum || Intent.WantTso) {
    InterlockedIncrement(&g_AerovNetDbgTxTcpCsumOffload);
  }
#endif

  // virtio-net uses a 10-byte header by default; when VIRTIO_NET_F_MRG_RXBUF is
  // negotiated, the header grows to 12 bytes (adding num_buffers). The TX side
  // still uses the same leading 10-byte layout, so zero the full header then
  // copy the base fields.
  RtlZeroMemory(TxReq->HeaderVa, Adapter->RxHeaderBytes);
  RtlCopyMemory(TxReq->HeaderVa, &BuiltHdr, sizeof(BuiltHdr));

  // Instrumentation: TX checksum offload usage by protocol.
  if (Info.L4Protocol == 6u) {
    if (Info.IpVersion == 4) {
      InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumOffloadTcp4);
    } else if (Info.IpVersion == 6) {
      InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumOffloadTcp6);
    }
  } else if (Info.L4Protocol == 17u) {
    if (Info.IpVersion == 4) {
      InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumOffloadUdp4);
    } else if (Info.IpVersion == 6) {
      InterlockedIncrement64((volatile LONG64*)&Adapter->StatTxCsumOffloadUdp6);
    }
  }

  return NDIS_STATUS_SUCCESS;
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

    {
      if (!TxReq->HeaderBuilt) {
        NDIS_STATUS TxStatus = AerovNetBuildTxHeader(Adapter, TxReq);
        if (TxStatus != NDIS_STATUS_SUCCESS) {
          RemoveEntryList(&TxReq->Link);
          InsertTailList(CompleteTxReqs, &TxReq->Link);
          AerovNetCompleteTxRequest(Adapter, TxReq, TxStatus, CompleteNblHead, CompleteNblTail);
          continue;
        }
        TxReq->HeaderBuilt = TRUE;
      }
    }

    Needed = (uint16_t)(TxReq->SgList->NumberOfElements + 1);

    Sg[0].addr = (uint64_t)TxReq->HeaderPa.QuadPart;
    Sg[0].len = (uint32_t)Adapter->RxHeaderBytes;
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

    Rx->PacketNext = NULL;
    Rx->PacketBytes = 0;

    // Expose only the Ethernet frame bytes to NDIS: the virtio-net header is
    // internal to the device/driver contract and is not part of the indicated frame.
    Rx->Mdl = IoAllocateMdl(Rx->BufferVa + Adapter->RxHeaderBytes, Rx->BufferBytes - Adapter->RxHeaderBytes, FALSE, FALSE, NULL);
    if (!Rx->Mdl) {
      return NDIS_STATUS_RESOURCES;
    }
    MmBuildMdlForNonPagedPool(Rx->Mdl);

    Rx->Nbl = NdisAllocateNetBufferAndNetBufferList(Adapter->NblPool, 0, 0, Rx->Mdl, 0, 0);
    if (!Rx->Nbl) {
      return NDIS_STATUS_RESOURCES;
    }

    Rx->Nb = NET_BUFFER_LIST_FIRST_NB(Rx->Nbl);
    Rx->Indicated = FALSE;

    Rx->Nbl->MiniportReserved[0] = Rx;

    InsertTailList(&Adapter->RxFreeList, &Rx->Link);
  }

  // Allocate a scratch buffer for checksum parsing on multi-buffer receives.
  // This avoids large stack allocations in the DPC path.
  Adapter->RxChecksumScratch = NULL;
  Adapter->RxChecksumScratchBytes = 0;
  if ((Adapter->GuestFeatures & VIRTIO_NET_F_MRG_RXBUF) != 0 && (Adapter->GuestFeatures & VIRTIO_NET_F_GUEST_CSUM) != 0 && Adapter->MaxFrameSize != 0) {
    Adapter->RxChecksumScratchBytes = Adapter->MaxFrameSize;
    Adapter->RxChecksumScratch =
        (PUCHAR)ExAllocatePoolWithTag(NonPagedPool, Adapter->RxChecksumScratchBytes, AEROVNET_TAG);
    if (!Adapter->RxChecksumScratch) {
      // Best-effort: checksum indication is optional. If allocation fails, we
      // will simply skip checksum parsing for multi-buffer frames.
      Adapter->RxChecksumScratchBytes = 0;
    } else {
      RtlZeroMemory(Adapter->RxChecksumScratch, Adapter->RxChecksumScratchBytes);
    }
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

  Adapter->TxHeaderBlockBytes = Adapter->RxHeaderBytes * Adapter->TxRequestCount;
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
    Tx->HeaderVa = Adapter->TxHeaderBlockVa + (Adapter->RxHeaderBytes * I);
    Tx->HeaderPa.QuadPart = Adapter->TxHeaderBlockPa.QuadPart + ((ULONGLONG)Adapter->RxHeaderBytes * (ULONGLONG)I);
    InsertTailList(&Adapter->TxFreeList, &Tx->Link);
  }

  return NDIS_STATUS_SUCCESS;
}

static NTSTATUS AerovNetProgramMsixVectorsInternal(_Inout_ AEROVNET_ADAPTER* Adapter,
                                                  _In_ USHORT ConfigVector,
                                                  _In_ USHORT RxVector,
                                                  _In_ USHORT TxVector) {
  NTSTATUS Status;

  if (!Adapter || !Adapter->Vdev.CommonCfg) {
    return STATUS_INVALID_PARAMETER;
  }

  Status = VirtioPciSetConfigMsixVector(&Adapter->Vdev, ConfigVector);
  if (!NT_SUCCESS(Status)) {
    return Status;
  }

  Status = VirtioPciSetQueueMsixVector(&Adapter->Vdev, 0, RxVector);
  if (!NT_SUCCESS(Status)) {
    return Status;
  }

  Status = VirtioPciSetQueueMsixVector(&Adapter->Vdev, 1, TxVector);
  if (!NT_SUCCESS(Status)) {
    return Status;
  }

  return STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetReregisterInterruptsIntx(_Inout_ AEROVNET_ADAPTER* Adapter) {
  NDIS_MINIPORT_INTERRUPT_CHARACTERISTICS Intr;
  NDIS_STATUS Status;
  NDIS_HANDLE OldHandle;

  if (!Adapter) {
    return NDIS_STATUS_FAILURE;
  }

  OldHandle = Adapter->InterruptHandle;
  Adapter->InterruptHandle = NULL;

  if (OldHandle) {
    NdisMDeregisterInterruptEx(OldHandle);
  }

  /*
   * Register legacy INTx interrupts only.
   *
   * We keep `Header.Revision=REVISION_2` for broad WDK compatibility, but leave
   * all message interrupt handlers NULL so NDIS will not connect MSI/MSI-X even
   * if the resource list contains message interrupts.
   *
   * This is critical for the contract v1 fallback path: when the PCI MSI-X
   * Enable bit is set, contract devices suppress INTx. If MSI-X vector
   * programming fails we must ensure Windows falls back to INTx at the PCI
   * layer (by not registering message interrupts) before proceeding.
   */
  RtlZeroMemory(&Intr, sizeof(Intr));
  Intr.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_INTERRUPT;
  Intr.Header.Revision = NDIS_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_2;
#ifdef NDIS_SIZEOF_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_2
  Intr.Header.Size = NDIS_SIZEOF_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_2;
#else
  Intr.Header.Size = sizeof(Intr);
#endif
  Intr.InterruptHandler = AerovNetInterruptIsr;
  Intr.InterruptDpcHandler = AerovNetInterruptDpc;
  Intr.MessageInterruptHandler = NULL;
  Intr.MessageInterruptDpcHandler = NULL;

  Status = NdisMRegisterInterruptEx(Adapter->MiniportAdapterHandle, Adapter, &Intr, &Adapter->InterruptHandle);
  return Status;
}

static NDIS_STATUS AerovNetProgramInterruptVectors(_Inout_ AEROVNET_ADAPTER* Adapter) {
  NTSTATUS NtStatus;
  USHORT ConfigVector;
  USHORT RxVector;
  USHORT TxVector;

  if (!Adapter || !Adapter->Vdev.CommonCfg) {
    return NDIS_STATUS_FAILURE;
  }

  if (!Adapter->UseMsix || Adapter->MsixMessageCount == 0) {
    // INTx: keep default virtio MSI-X routing disabled.
    (VOID)VirtioPciSetConfigMsixVector(&Adapter->Vdev, VIRTIO_PCI_MSI_NO_VECTOR);
    (VOID)VirtioPciSetQueueMsixVector(&Adapter->Vdev, 0, VIRTIO_PCI_MSI_NO_VECTOR);
    (VOID)VirtioPciSetQueueMsixVector(&Adapter->Vdev, 1, VIRTIO_PCI_MSI_NO_VECTOR);

    Adapter->MsixAllOnVector0 = FALSE;
    Adapter->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    Adapter->MsixRxVector = VIRTIO_PCI_MSI_NO_VECTOR;
    Adapter->MsixTxVector = VIRTIO_PCI_MSI_NO_VECTOR;
    return NDIS_STATUS_SUCCESS;
  }

  ConfigVector = Adapter->MsixConfigVector;
  RxVector = Adapter->MsixRxVector;
  TxVector = Adapter->MsixTxVector;

  NtStatus = AerovNetProgramMsixVectorsInternal(Adapter, ConfigVector, RxVector, TxVector);
  if (NT_SUCCESS(NtStatus)) {
    return NDIS_STATUS_SUCCESS;
  }

  if (Adapter->MsixAllOnVector0) {
    DbgPrintEx(DPFLTR_IHVDRIVER_ID,
               DPFLTR_ERROR_LEVEL,
               "aero_virtio_net: MSI-X vector programming failed (cfg=%hu rx=%hu tx=%hu messages=%hu status=%!STATUS!)\n",
               ConfigVector,
               RxVector,
               TxVector,
               Adapter->MsixMessageCount,
               NtStatus);
    // Contract v1 fallback: keep the adapter functional by reverting to legacy INTx.
    Adapter->MsixVectorProgrammingFailed = TRUE;
    Adapter->UseMsix = FALSE;
    Adapter->MsixAllOnVector0 = FALSE;
    Adapter->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    Adapter->MsixRxVector = VIRTIO_PCI_MSI_NO_VECTOR;
    Adapter->MsixTxVector = VIRTIO_PCI_MSI_NO_VECTOR;
    (VOID)VirtioPciSetConfigMsixVector(&Adapter->Vdev, VIRTIO_PCI_MSI_NO_VECTOR);
    (VOID)VirtioPciSetQueueMsixVector(&Adapter->Vdev, 0, VIRTIO_PCI_MSI_NO_VECTOR);
    (VOID)VirtioPciSetQueueMsixVector(&Adapter->Vdev, 1, VIRTIO_PCI_MSI_NO_VECTOR);
    return AerovNetReregisterInterruptsIntx(Adapter);
  }

  DbgPrintEx(DPFLTR_IHVDRIVER_ID,
             DPFLTR_ERROR_LEVEL,
             "aero_virtio_net: MSI-X vector programming failed (cfg=%hu rx=%hu tx=%hu messages=%hu status=%!STATUS!), falling back to vector0\n",
             ConfigVector,
             RxVector,
             TxVector,
             Adapter->MsixMessageCount,
             NtStatus);

  // Required fallback: route config + all queues to vector 0.
  Adapter->MsixAllOnVector0 = TRUE;
  Adapter->MsixConfigVector = 0;
  Adapter->MsixRxVector = 0;
  Adapter->MsixTxVector = 0;

  NtStatus = AerovNetProgramMsixVectorsInternal(Adapter, 0, 0, 0);
  if (NT_SUCCESS(NtStatus)) {
    return NDIS_STATUS_SUCCESS;
  }

  DbgPrintEx(DPFLTR_IHVDRIVER_ID,
             DPFLTR_ERROR_LEVEL,
             "aero_virtio_net: MSI-X vector0 fallback failed (messages=%hu status=%!STATUS!)\n",
             Adapter->MsixMessageCount,
             NtStatus);
  // Contract v1 fallback: keep the adapter functional by reverting to legacy INTx.
  Adapter->MsixVectorProgrammingFailed = TRUE;
  Adapter->UseMsix = FALSE;
  Adapter->MsixAllOnVector0 = FALSE;
  Adapter->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
  Adapter->MsixRxVector = VIRTIO_PCI_MSI_NO_VECTOR;
  Adapter->MsixTxVector = VIRTIO_PCI_MSI_NO_VECTOR;
  (VOID)VirtioPciSetConfigMsixVector(&Adapter->Vdev, VIRTIO_PCI_MSI_NO_VECTOR);
  (VOID)VirtioPciSetQueueMsixVector(&Adapter->Vdev, 0, VIRTIO_PCI_MSI_NO_VECTOR);
  (VOID)VirtioPciSetQueueMsixVector(&Adapter->Vdev, 1, VIRTIO_PCI_MSI_NO_VECTOR);
  return AerovNetReregisterInterruptsIntx(Adapter);
}

static NDIS_STATUS AerovNetSetupVq(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_VQ* Vq, _In_ USHORT QueueIndex,
                                  _In_ USHORT ExpectedQueueSize, _In_ USHORT IndirectMaxDesc) {
  USHORT QueueSize;
  NTSTATUS NtStatus;
  int VqRes;
  virtio_bool_t UseIndirect;
  virtio_bool_t EventIdx;
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
  if (ExpectedQueueSize != 0 && QueueSize != ExpectedQueueSize) {
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

  EventIdx = (Adapter->GuestFeatures & AEROVNET_FEATURE_RING_EVENT_IDX) ? VIRTIO_TRUE : VIRTIO_FALSE;

  VqRes = virtqueue_split_alloc_ring(&Adapter->VirtioOps, &Adapter->VirtioOpsCtx, QueueSize, 16, EventIdx, &Vq->RingDma);
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
                                EventIdx,
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
                                  EventIdx,
                                  VIRTIO_FALSE,
                                  0);
  }

  if (VqRes != VIRTIO_OK) {
    return NDIS_STATUS_RESOURCES;
  }

  DescPa = Vq->RingDma.paddr + (UINT64)((PUCHAR)Vq->Vq.desc - (PUCHAR)Vq->RingDma.vaddr);
  AvailPa = Vq->RingDma.paddr + (UINT64)((PUCHAR)Vq->Vq.avail - (PUCHAR)Vq->RingDma.vaddr);
  UsedPa = Vq->RingDma.paddr + (UINT64)((PUCHAR)Vq->Vq.used - (PUCHAR)Vq->RingDma.vaddr);

  NtStatus = VirtioPciSetupQueue(&Adapter->Vdev, QueueIndex, DescPa, AvailPa, UsedPa);
  if (!NT_SUCCESS(NtStatus)) {
    return NDIS_STATUS_FAILURE;
  }

  return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetCtrlVqRegistryWriteDword(_In_ HANDLE Key, _In_ PCWSTR Name, _In_ ULONG Value) {
  UNICODE_STRING ValueName;

  if (Key == NULL || Name == NULL) {
    return;
  }

  RtlInitUnicodeString(&ValueName, Name);
  (VOID)ZwSetValueKey(Key, &ValueName, 0, REG_DWORD, &Value, sizeof(Value));
}

static VOID AerovNetCtrlVqRegistryWriteQword(_In_ HANDLE Key, _In_ PCWSTR Name, _In_ ULONGLONG Value) {
  UNICODE_STRING ValueName;

  if (Key == NULL || Name == NULL) {
    return;
  }

  RtlInitUnicodeString(&ValueName, Name);
  (VOID)ZwSetValueKey(Key, &ValueName, 0, REG_QWORD, &Value, sizeof(Value));
}

static BOOLEAN AerovNetCtrlVqRegistryReadDword(_In_ HANDLE Key, _In_ PCWSTR Name, _Out_ ULONG* ValueOut) {
  UNICODE_STRING ValueName;
  UCHAR Buf[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + sizeof(ULONG)];
  PKEY_VALUE_PARTIAL_INFORMATION Info;
  ULONG ResultLen;
  NTSTATUS Status;

  if (ValueOut) {
    *ValueOut = 0;
  }

  if (Key == NULL || Name == NULL || ValueOut == NULL) {
    return FALSE;
  }

  Info = (PKEY_VALUE_PARTIAL_INFORMATION)Buf;
  RtlZeroMemory(Info, sizeof(Buf));
  ResultLen = 0;

  RtlInitUnicodeString(&ValueName, Name);
  Status = ZwQueryValueKey(Key, &ValueName, KeyValuePartialInformation, Info, sizeof(Buf), &ResultLen);
  if (!NT_SUCCESS(Status)) {
    return FALSE;
  }
  if (Info->Type != REG_DWORD || Info->DataLength != sizeof(ULONG)) {
    return FALSE;
  }
  RtlCopyMemory(ValueOut, Info->Data, sizeof(ULONG));
  return TRUE;
}

static BOOLEAN AerovNetCtrlVqRegistryReadMultiSz(_In_ HANDLE Key, _In_ PCWSTR Name, _Outptr_result_bytebuffer_(*BytesOut) PWCHAR* ValueOut,
                                                _Out_ ULONG* BytesOut) {
  UNICODE_STRING ValueName;
  ULONG ResultLen;
  NTSTATUS Status;
  UCHAR SmallBuf[sizeof(KEY_VALUE_PARTIAL_INFORMATION)];
  PKEY_VALUE_PARTIAL_INFORMATION Info;
  BOOLEAN NeedFree;
  ULONG AllocBytes;
  ULONG DataBytes;
  PWCHAR Copy;

  if (ValueOut) {
    *ValueOut = NULL;
  }
  if (BytesOut) {
    *BytesOut = 0;
  }

  if (Key == NULL || Name == NULL || ValueOut == NULL || BytesOut == NULL) {
    return FALSE;
  }

  RtlInitUnicodeString(&ValueName, Name);
  ResultLen = 0;
  Info = (PKEY_VALUE_PARTIAL_INFORMATION)SmallBuf;
  NeedFree = FALSE;
  RtlZeroMemory(SmallBuf, sizeof(SmallBuf));
  Status = ZwQueryValueKey(Key, &ValueName, KeyValuePartialInformation, Info, sizeof(SmallBuf), &ResultLen);
  if (Status == STATUS_BUFFER_TOO_SMALL || Status == STATUS_BUFFER_OVERFLOW) {
    AllocBytes = ResultLen;
    Info = (PKEY_VALUE_PARTIAL_INFORMATION)ExAllocatePoolWithTag(NonPagedPool, AllocBytes, AEROVNET_TAG);
    if (!Info) {
      return FALSE;
    }
    NeedFree = TRUE;
    RtlZeroMemory(Info, AllocBytes);
    ResultLen = 0;
    Status = ZwQueryValueKey(Key, &ValueName, KeyValuePartialInformation, Info, AllocBytes, &ResultLen);
  }
  if (!NT_SUCCESS(Status)) {
    if (NeedFree) {
      ExFreePoolWithTag(Info, AEROVNET_TAG);
    }
    return FALSE;
  }

  DataBytes = Info->DataLength;
  if (Info->Type != REG_MULTI_SZ || DataBytes < sizeof(WCHAR) || (DataBytes % sizeof(WCHAR)) != 0) {
    if (NeedFree) {
      ExFreePoolWithTag(Info, AEROVNET_TAG);
    }
    return FALSE;
  }

  // Copy out the MULTI_SZ payload and ensure it is double-NUL terminated so the
  // parser cannot read past the allocation if the registry data is malformed
  // (e.g. missing a trailing empty string terminator).
  Copy = (PWCHAR)ExAllocatePoolWithTag(NonPagedPool, DataBytes + (2 * sizeof(WCHAR)), AEROVNET_TAG);
  if (!Copy) {
    if (NeedFree) {
      ExFreePoolWithTag(Info, AEROVNET_TAG);
    }
    return FALSE;
  }
  RtlZeroMemory(Copy, DataBytes + (2 * sizeof(WCHAR)));
  RtlCopyMemory(Copy, Info->Data, DataBytes);

  if (NeedFree) {
    ExFreePoolWithTag(Info, AEROVNET_TAG);
  }
  *ValueOut = Copy;
  *BytesOut = DataBytes;
  return TRUE;
}

static VOID AerovNetCtrlVqRegistryUpdate(_Inout_ AEROVNET_ADAPTER* Adapter) {
  HANDLE Key;
  ULONGLONG HostFeatures;
  ULONGLONG GuestFeatures;
  ULONG CtrlVqNegotiated;
  ULONG CtrlRxNegotiated;
  ULONG CtrlVlanNegotiated;
  ULONG CtrlMacAddrNegotiated;
  ULONG CtrlVqQueueIndex;
  ULONG CtrlVqQueueSize;
  ULONGLONG CmdSent;
  ULONGLONG CmdOk;
  ULONGLONG CmdErr;
  ULONGLONG CmdTimeout;

  if (!Adapter) {
    return;
  }

  Key = Adapter->CtrlVqRegKey;
  if (Key == NULL) {
    return;
  }

  /*
   * Snapshot diagnostics under the adapter lock so:
   * - 64-bit fields don't tear on x86
   * - the registry values are mutually consistent
   *
   * Do not write to the registry while holding the spin lock.
   */
  NdisAcquireSpinLock(&Adapter->Lock);
  HostFeatures = (ULONGLONG)Adapter->HostFeatures;
  GuestFeatures = (ULONGLONG)Adapter->GuestFeatures;
  CtrlVqNegotiated = (GuestFeatures & VIRTIO_NET_F_CTRL_VQ) ? 1u : 0u;
  CtrlRxNegotiated = (GuestFeatures & VIRTIO_NET_F_CTRL_RX) ? 1u : 0u;
  CtrlVlanNegotiated = (GuestFeatures & VIRTIO_NET_F_CTRL_VLAN) ? 1u : 0u;
  CtrlMacAddrNegotiated = (GuestFeatures & VIRTIO_NET_F_CTRL_MAC_ADDR) ? 1u : 0u;

  CtrlVqQueueIndex = (ULONG)Adapter->CtrlVq.QueueIndex;
  CtrlVqQueueSize = (ULONG)Adapter->CtrlVq.QueueSize;

  CmdSent = Adapter->StatCtrlVqCmdSent;
  CmdOk = Adapter->StatCtrlVqCmdOk;
  CmdErr = Adapter->StatCtrlVqCmdErr;
  CmdTimeout = Adapter->StatCtrlVqCmdTimeout;
  NdisReleaseSpinLock(&Adapter->Lock);

  AerovNetCtrlVqRegistryWriteQword(Key, L"HostFeatures", HostFeatures);
  AerovNetCtrlVqRegistryWriteQword(Key, L"GuestFeatures", GuestFeatures);

  AerovNetCtrlVqRegistryWriteDword(Key, L"CtrlVqNegotiated", CtrlVqNegotiated);
  AerovNetCtrlVqRegistryWriteDword(Key, L"CtrlRxNegotiated", CtrlRxNegotiated);
  AerovNetCtrlVqRegistryWriteDword(Key, L"CtrlVlanNegotiated", CtrlVlanNegotiated);
  AerovNetCtrlVqRegistryWriteDword(Key, L"CtrlMacAddrNegotiated", CtrlMacAddrNegotiated);

  AerovNetCtrlVqRegistryWriteDword(Key, L"CtrlVqQueueIndex", CtrlVqQueueIndex);
  AerovNetCtrlVqRegistryWriteDword(Key, L"CtrlVqQueueSize", CtrlVqQueueSize);

  AerovNetCtrlVqRegistryWriteQword(Key, L"CtrlVqCmdSent", CmdSent);
  AerovNetCtrlVqRegistryWriteQword(Key, L"CtrlVqCmdOk", CmdOk);
  AerovNetCtrlVqRegistryWriteQword(Key, L"CtrlVqCmdErr", CmdErr);
  AerovNetCtrlVqRegistryWriteQword(Key, L"CtrlVqCmdTimeout", CmdTimeout);
}

static VOID AerovNetCtrlVqRegistryInit(_Inout_ AEROVNET_ADAPTER* Adapter) {
  NTSTATUS Status;
  PDEVICE_OBJECT Pdo;
  HANDLE DevKey;
  HANDLE Key;
  UNICODE_STRING SubkeyName;
  OBJECT_ATTRIBUTES Oa;

  if (!Adapter) {
    return;
  }
  if (Adapter->CtrlVqRegKey != NULL) {
    return;
  }

  Pdo = NULL;
  DevKey = NULL;
  Key = NULL;

  NdisMGetDeviceProperty(Adapter->MiniportAdapterHandle, &Pdo, NULL, NULL, NULL, NULL);
  if (Pdo == NULL) {
    return;
  }

  Status = IoOpenDeviceRegistryKey(Pdo, PLUGPLAY_REGKEY_DEVICE, KEY_CREATE_SUB_KEY | KEY_SET_VALUE, &DevKey);
  if (!NT_SUCCESS(Status) || DevKey == NULL) {
    return;
  }

  RtlInitUnicodeString(&SubkeyName, L"Device Parameters\\AeroVirtioNet");
  InitializeObjectAttributes(&Oa, &SubkeyName, OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE, DevKey, NULL);

  Status = ZwCreateKey(&Key, KEY_SET_VALUE | KEY_QUERY_VALUE, &Oa, 0, NULL, REG_OPTION_NON_VOLATILE, NULL);
  ZwClose(DevKey);
  DevKey = NULL;

  if (!NT_SUCCESS(Status) || Key == NULL) {
    return;
  }

  Adapter->CtrlVqRegKey = Key;
  AerovNetCtrlVqRegistryUpdate(Adapter);
}

static BOOLEAN AerovNetParseDecimalUlong(_In_z_ const WCHAR* Str, _Out_ ULONG* ValueOut) {
  const WCHAR* S;
  ULONG V;
  BOOLEAN HaveDigit;

  if (ValueOut) {
    *ValueOut = 0;
  }

  if (Str == NULL || ValueOut == NULL) {
    return FALSE;
  }

  S = Str;
  while (*S == L' ' || *S == L'\t') {
    S++;
  }

  V = 0;
  HaveDigit = FALSE;
  while (*S >= L'0' && *S <= L'9') {
    ULONG Digit = (ULONG)(*S - L'0');
    if (V > (0xFFFFFFFFu - Digit) / 10u) {
      return FALSE;
    }
    V = (V * 10u) + Digit;
    HaveDigit = TRUE;
    S++;
  }

  while (*S == L' ' || *S == L'\t') {
    S++;
  }

  if (!HaveDigit || *S != L'\0') {
    return FALSE;
  }

  *ValueOut = V;
  return TRUE;
}

static VOID AerovNetCtrlVlanConfigureFromRegistry(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG VlanId;
  PWCHAR VlanIds;
  ULONG VlanIdsBytes;
  NDIS_STATUS Status;
  const WCHAR* P;
  USHORT VidList[64];
  ULONG VidCount;
  ULONG I;
  ULONG MaxVidCount;

  if (!Adapter) {
    return;
  }

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VLAN) == 0) {
    return;
  }

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VQ) == 0 || Adapter->CtrlVq.QueueSize == 0) {
    return;
  }

  if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
    return;
  }

  // Optional configuration knob: if the per-device registry key contains a
  // `VlanIds` MULTI_SZ (or legacy `VlanId` DWORD), add it to the device VLAN
  // filter table via ctrl_vq.
  //
  // This is best-effort and is only intended for device models that expose
  // virtio-net VLAN filtering (VIRTIO_NET_F_CTRL_VLAN). If unset, the driver
  // does not configure VLAN filtering and continues to accept VLAN-tagged frames
  // via software.

  // Newer configuration: multi-string list of VLAN IDs.
  //
  // If present, the legacy single VlanId DWORD is ignored.
  VlanIds = NULL;
  VlanIdsBytes = 0;
  if (AerovNetCtrlVqRegistryReadMultiSz(Adapter->CtrlVqRegKey, L"VlanIds", &VlanIds, &VlanIdsBytes)) {
    MaxVidCount = (ULONG)(sizeof(VidList) / sizeof(VidList[0]));
    VidCount = 0;
    P = VlanIds;
    while (P && *P) {
      ULONG Parsed;
      ULONG Len;
      BOOLEAN Duplicate;

      // Compute length of current string.
      Len = 0;
      while (P[Len] != L'\0') {
        Len++;
      }

      Parsed = 0;
      (VOID)AerovNetParseDecimalUlong(P, &Parsed);

      if (Parsed != 0 && Parsed < 4095u) {
        USHORT Vid = (USHORT)Parsed;
        Duplicate = FALSE;
        for (I = 0; I < VidCount; I++) {
          if (VidList[I] == Vid) {
            Duplicate = TRUE;
            break;
          }
        }
        if (!Duplicate && VidCount < MaxVidCount) {
          VidList[VidCount++] = Vid;
        }
      }

      P += Len + 1;
    }

    ExFreePoolWithTag(VlanIds, AEROVNET_TAG);
    VlanIds = NULL;

    for (I = 0; I < VidCount; I++) {
      Status = AerovNetCtrlVlanUpdate(Adapter, TRUE, VidList[I]);
#if DBG
      DbgPrint("virtio-net-ctrl-vq|INFO|vlan_add|vid=%hu|status=0x%08x\n", VidList[I], Status);
#endif
    }

    return;
  }

  VlanId = 0;
  if (!AerovNetCtrlVqRegistryReadDword(Adapter->CtrlVqRegKey, L"VlanId", &VlanId)) {
    return;
  }

  if (VlanId == 0 || VlanId >= 4095u) {
    return;
  }

  Status = AerovNetCtrlVlanUpdate(Adapter, TRUE, (USHORT)VlanId);
#if DBG
  DbgPrint("virtio-net-ctrl-vq|INFO|vlan_add|vid=%lu|status=0x%08x\n", VlanId, Status);
#endif
}

typedef struct _AEROVNET_CTRL_REQUEST {
  LIST_ENTRY Link;
  UCHAR Class;
  UCHAR Command;
  UCHAR Ack;
  BOOLEAN Completed;

  PUCHAR BufferVa;
  PHYSICAL_ADDRESS BufferPa;
  ULONG BufferBytes;
  ULONG CmdBytes;
} AEROVNET_CTRL_REQUEST;

static VOID AerovNetCtrlFreeRequest(_Inout_ AEROVNET_CTRL_REQUEST* Req) {
  if (!Req) {
    return;
  }

  if (Req->BufferVa) {
    if (Req->BufferBytes != 0) {
      MmFreeContiguousMemorySpecifyCache(Req->BufferVa, Req->BufferBytes, MmCached);
    }
    Req->BufferVa = NULL;
    Req->BufferBytes = 0;
    Req->BufferPa.QuadPart = 0;
    Req->CmdBytes = 0;
  }

  ExFreePoolWithTag(Req, AEROVNET_TAG);
}

static VOID AerovNetFreeCtrlPendingRequests(_Inout_ AEROVNET_ADAPTER* Adapter) {
  LIST_ENTRY Pending;

  if (!Adapter) {
    return;
  }

  InitializeListHead(&Pending);

  NdisAcquireSpinLock(&Adapter->Lock);
  while (!IsListEmpty(&Adapter->CtrlPendingList)) {
    PLIST_ENTRY E = RemoveHeadList(&Adapter->CtrlPendingList);
    AEROVNET_CTRL_REQUEST* Req = CONTAINING_RECORD(E, AEROVNET_CTRL_REQUEST, Link);
    Req->Link.Flink = NULL;
    Req->Link.Blink = NULL;
    InsertTailList(&Pending, &Req->Link);
  }
  InitializeListHead(&Adapter->CtrlPendingList);
  NdisReleaseSpinLock(&Adapter->Lock);

  while (!IsListEmpty(&Pending)) {
    PLIST_ENTRY E = RemoveHeadList(&Pending);
    AEROVNET_CTRL_REQUEST* Req = CONTAINING_RECORD(E, AEROVNET_CTRL_REQUEST, Link);
    AerovNetCtrlFreeRequest(Req);
  }
}

static VOID AerovNetCtrlCollectUsedLocked(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ LIST_ENTRY* CompletedList) {
  if (!Adapter || !CompletedList) {
    return;
  }

  if (Adapter->CtrlVq.QueueSize == 0) {
    return;
  }

  for (;;) {
    PVOID Cookie;

    Cookie = NULL;
    if (virtqueue_split_pop_used(&Adapter->CtrlVq.Vq, &Cookie, NULL) == VIRTIO_FALSE) {
      break;
    }

    if (!Cookie) {
      continue;
    }

    {
      AEROVNET_CTRL_REQUEST* Req = (AEROVNET_CTRL_REQUEST*)Cookie;

      if (Req->Link.Flink && Req->Link.Blink) {
        RemoveEntryList(&Req->Link);
        Req->Link.Flink = NULL;
        Req->Link.Blink = NULL;
      }

      Req->Completed = TRUE;
      Req->Ack = VIRTIO_NET_ERR;
      KeMemoryBarrier();
      if (Req->BufferVa && Req->CmdBytes + sizeof(UCHAR) <= Req->BufferBytes) {
        Req->Ack = *(volatile UCHAR*)(Req->BufferVa + Req->CmdBytes);
      }

      if (Req->Ack == VIRTIO_NET_OK) {
        Adapter->StatCtrlVqCmdOk++;
      } else {
        Adapter->StatCtrlVqCmdErr++;
      }

      InsertTailList(CompletedList, &Req->Link);
    }
  }
}

static NDIS_STATUS AerovNetCtrlSendCommand(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ UCHAR Class, _In_ UCHAR Command,
                                          _In_reads_bytes_opt_(DataBytes) const VOID* Data, _In_ USHORT DataBytes, _Out_opt_ UCHAR* AckOut) {
  NDIS_STATUS Status;
  NTSTATUS WaitStatus;
  AEROVNET_CTRL_REQUEST* Req;
  ULONG CmdBytes;
  ULONG TotalBytes;
  PHYSICAL_ADDRESS Low;
  PHYSICAL_ADDRESS High;
  PHYSICAL_ADDRESS Skip;
  virtio_sg_entry_t Sg[2];
  virtio_bool_t UseIndirect;
  uint16_t Head;
  int VqRes;
  ULONGLONG Deadline100ns;
  BOOLEAN Done;
  UCHAR FinalAck;

  if (AckOut) {
    *AckOut = VIRTIO_NET_ERR;
  }

  if (!Adapter) {
    return NDIS_STATUS_FAILURE;
  }

  if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
    return NDIS_STATUS_FAILURE;
  }

  if (Adapter->SurpriseRemoved) {
    return NDIS_STATUS_RESET_IN_PROGRESS;
  }

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VQ) == 0 || Adapter->CtrlVq.QueueSize == 0) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  if (DataBytes != 0 && Data == NULL) {
    return NDIS_STATUS_INVALID_DATA;
  }

  // Serialize synchronous control commands. AerovNetCtrlSendCommand drains and
  // frees completed requests; concurrent callers could free each other's
  // requests, resulting in spurious timeouts and use-after-free.
  WaitStatus = KeWaitForSingleObject(&Adapter->CtrlCmdEvent, Executive, KernelMode, FALSE, NULL);
  if (WaitStatus != STATUS_SUCCESS) {
    return NDIS_STATUS_FAILURE;
  }

  if (Adapter->SurpriseRemoved) {
    Status = NDIS_STATUS_RESET_IN_PROGRESS;
    goto Exit;
  }

  CmdBytes = sizeof(VIRTIO_NET_CTRL_HDR) + (ULONG)DataBytes;
  TotalBytes = CmdBytes + sizeof(UCHAR); // ack

  Req = (AEROVNET_CTRL_REQUEST*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*Req), AEROVNET_TAG);
  if (!Req) {
    Status = NDIS_STATUS_RESOURCES;
    goto Exit;
  }
  RtlZeroMemory(Req, sizeof(*Req));
  Req->Link.Flink = NULL;
  Req->Link.Blink = NULL;
  Req->Class = Class;
  Req->Command = Command;
  Req->Ack = VIRTIO_NET_ERR;
  Req->Completed = FALSE;
  Req->CmdBytes = CmdBytes;

  Low.QuadPart = 0;
  High.QuadPart = ~0ull;
  Skip.QuadPart = 0;

  Req->BufferBytes = TotalBytes;
  Req->BufferVa = MmAllocateContiguousMemorySpecifyCache(Req->BufferBytes, Low, High, Skip, MmCached);
  if (!Req->BufferVa) {
    AerovNetCtrlFreeRequest(Req);
    Status = NDIS_STATUS_RESOURCES;
    goto Exit;
  }
  Req->BufferPa = MmGetPhysicalAddress(Req->BufferVa);
  RtlZeroMemory(Req->BufferVa, Req->BufferBytes);

  {
    VIRTIO_NET_CTRL_HDR* Hdr = (VIRTIO_NET_CTRL_HDR*)Req->BufferVa;
    Hdr->Class = Class;
    Hdr->Command = Command;
    if (DataBytes) {
      RtlCopyMemory(Req->BufferVa + sizeof(*Hdr), Data, DataBytes);
    }
    *(Req->BufferVa + CmdBytes) = 0xFF; // ack sentinel
  }

  Sg[0].addr = (uint64_t)Req->BufferPa.QuadPart;
  Sg[0].len = (uint32_t)CmdBytes;
  Sg[0].device_writes = VIRTIO_FALSE;

  Sg[1].addr = (uint64_t)Req->BufferPa.QuadPart + (uint64_t)CmdBytes;
  Sg[1].len = (uint32_t)sizeof(UCHAR);
  Sg[1].device_writes = VIRTIO_TRUE;

  Status = NDIS_STATUS_SUCCESS;
  Done = FALSE;
  FinalAck = VIRTIO_NET_ERR;

  NdisAcquireSpinLock(&Adapter->Lock);

  // First drain any completed control commands to keep descriptors available.
  {
    LIST_ENTRY Completed;
    InitializeListHead(&Completed);
    AerovNetCtrlCollectUsedLocked(Adapter, &Completed);

    NdisReleaseSpinLock(&Adapter->Lock);

    while (!IsListEmpty(&Completed)) {
      PLIST_ENTRY E = RemoveHeadList(&Completed);
      AEROVNET_CTRL_REQUEST* Old = CONTAINING_RECORD(E, AEROVNET_CTRL_REQUEST, Link);
      AerovNetCtrlFreeRequest(Old);
    }

    NdisAcquireSpinLock(&Adapter->Lock);
  }

  InsertTailList(&Adapter->CtrlPendingList, &Req->Link);

  UseIndirect = (Adapter->CtrlVq.Vq.indirect_desc != VIRTIO_FALSE) ? VIRTIO_TRUE : VIRTIO_FALSE;
  Head = 0;
  VqRes = virtqueue_split_add_sg(&Adapter->CtrlVq.Vq, Sg, 2, Req, UseIndirect, &Head);
  if (VqRes != VIRTIO_OK) {
    RemoveEntryList(&Req->Link);
    Req->Link.Flink = NULL;
    Req->Link.Blink = NULL;
    Status = NDIS_STATUS_RESOURCES;
  } else {
    Adapter->StatCtrlVqCmdSent++;
    UNREFERENCED_PARAMETER(Head);

    if (AerovNetVirtqueueKickPrepareContractV1(&Adapter->CtrlVq.Vq) != VIRTIO_FALSE) {
      KeMemoryBarrier();
      if (!Adapter->SurpriseRemoved) {
        VirtioPciNotifyQueue(&Adapter->Vdev, Adapter->CtrlVq.QueueIndex);
      }
    }
  }

  NdisReleaseSpinLock(&Adapter->Lock);

  if (Status != NDIS_STATUS_SUCCESS) {
    AerovNetCtrlFreeRequest(Req);
    AerovNetCtrlVqRegistryUpdate(Adapter);
    goto Exit;
  }

  // Poll for completion (interrupts may be suppressed during init while Adapter->State is Stopped).
  Deadline100ns = KeQueryInterruptTime() + (ULONGLONG)10 * 1000ull * 1000ull; // 1s
  for (;;) {
    LIST_ENTRY Completed;
    InitializeListHead(&Completed);

    if (Adapter->SurpriseRemoved) {
      Status = NDIS_STATUS_RESET_IN_PROGRESS;
      goto Exit;
    }

    NdisAcquireSpinLock(&Adapter->Lock);
    AerovNetCtrlCollectUsedLocked(Adapter, &Completed);
    NdisReleaseSpinLock(&Adapter->Lock);

    while (!IsListEmpty(&Completed)) {
      PLIST_ENTRY E = RemoveHeadList(&Completed);
      AEROVNET_CTRL_REQUEST* DoneReq = CONTAINING_RECORD(E, AEROVNET_CTRL_REQUEST, Link);

      if (DoneReq == Req) {
        Done = TRUE;
        FinalAck = DoneReq->Ack;
      }

      AerovNetCtrlFreeRequest(DoneReq);
    }

    if (Done) {
      if (AckOut) {
        *AckOut = FinalAck;
      }
      Status = (FinalAck == VIRTIO_NET_OK) ? NDIS_STATUS_SUCCESS : NDIS_STATUS_FAILURE;
      AerovNetCtrlVqRegistryUpdate(Adapter);
      goto Exit;
    }

    if (KeQueryInterruptTime() >= Deadline100ns) {
      NdisAcquireSpinLock(&Adapter->Lock);
      Adapter->StatCtrlVqCmdTimeout++;
      NdisReleaseSpinLock(&Adapter->Lock);
      AerovNetCtrlVqRegistryUpdate(Adapter);
      Status = NDIS_STATUS_FAILURE;
      goto Exit;
    }

    {
      LARGE_INTEGER Interval;
      Interval.QuadPart = -10 * 1000; // 1ms
      (VOID)KeDelayExecutionThread(KernelMode, FALSE, &Interval);
    }
  }

Exit:
  KeSetEvent(&Adapter->CtrlCmdEvent, IO_NO_INCREMENT, FALSE);
  return Status;
}

static NDIS_STATUS AerovNetCtrlSetMac(_Inout_ AEROVNET_ADAPTER* Adapter, _In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* Mac) {
  if (!Adapter || !Mac) {
    return NDIS_STATUS_FAILURE;
  }
  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_MAC_ADDR) == 0) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }
  return AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_MAC, VIRTIO_NET_CTRL_MAC_ADDR_SET, Mac, ETH_LENGTH_OF_ADDRESS, NULL);
}

static NDIS_STATUS AerovNetCtrlVlanUpdate(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ BOOLEAN Add, _In_ USHORT VlanId) {
  USHORT LeVid;

  if (!Adapter) {
    return NDIS_STATUS_FAILURE;
  }
  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VLAN) == 0) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  LeVid = VlanId;
  return AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_VLAN, Add ? VIRTIO_NET_CTRL_VLAN_ADD : VIRTIO_NET_CTRL_VLAN_DEL, &LeVid,
                                 sizeof(LeVid), NULL);
}

static NDIS_STATUS AerovNetCtrlSetMacTable(_Inout_ AEROVNET_ADAPTER* Adapter, _In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* UnicastMac,
                                           _In_ ULONG MulticastCount,
                                           _In_reads_bytes_opt_(MulticastCount * ETH_LENGTH_OF_ADDRESS) const UCHAR* MulticastMacs) {
  NDIS_STATUS Status;
  ULONG DataBytes;
  PUCHAR Data;
  ULONG Offset;
  UINT32 Entries;
  USHORT DataBytesU16;

  if (!Adapter || !UnicastMac) {
    return NDIS_STATUS_FAILURE;
  }

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_RX) == 0) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  if (MulticastCount != 0 && MulticastMacs == NULL) {
    return NDIS_STATUS_INVALID_DATA;
  }

  // Payload layout for VIRTIO_NET_CTRL_MAC_TABLE_SET:
  //   u32 unicast_entries
  //   u8  unicast_macs[unicast_entries][6]
  //   u32 multicast_entries
  //   u8  multicast_macs[multicast_entries][6]
  DataBytes = sizeof(UINT32) + ETH_LENGTH_OF_ADDRESS + sizeof(UINT32) + (MulticastCount * ETH_LENGTH_OF_ADDRESS);
  if (DataBytes > 0xFFFFu) {
    return NDIS_STATUS_INVALID_LENGTH;
  }
  DataBytesU16 = (USHORT)DataBytes;

  Data = (PUCHAR)ExAllocatePoolWithTag(NonPagedPool, DataBytes, AEROVNET_TAG);
  if (!Data) {
    return NDIS_STATUS_RESOURCES;
  }
  RtlZeroMemory(Data, DataBytes);

  Offset = 0;
  Entries = 1;
  RtlCopyMemory(Data + Offset, &Entries, sizeof(Entries));
  Offset += sizeof(Entries);

  RtlCopyMemory(Data + Offset, UnicastMac, ETH_LENGTH_OF_ADDRESS);
  Offset += ETH_LENGTH_OF_ADDRESS;

  Entries = (UINT32)MulticastCount;
  RtlCopyMemory(Data + Offset, &Entries, sizeof(Entries));
  Offset += sizeof(Entries);

  if (MulticastCount) {
    RtlCopyMemory(Data + Offset, MulticastMacs, MulticastCount * ETH_LENGTH_OF_ADDRESS);
    Offset += MulticastCount * ETH_LENGTH_OF_ADDRESS;
  }

  ASSERT(Offset == DataBytes);

  Status = AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_MAC, VIRTIO_NET_CTRL_MAC_TABLE_SET, Data, DataBytesU16, NULL);

  ExFreePoolWithTag(Data, AEROVNET_TAG);
  return Status;
}

static VOID AerovNetCtrlUpdateRxMode(_Inout_ AEROVNET_ADAPTER* Adapter) {
  ULONG Filter;
  ULONG MulticastCount;
  UCHAR MulticastMacs[NDIS_MAX_MULTICAST_LIST][ETH_LENGTH_OF_ADDRESS];
  UCHAR UnicastMac[ETH_LENGTH_OF_ADDRESS];
  UCHAR On;
  BOOLEAN WantPromisc;
  BOOLEAN WantUnicast;
  BOOLEAN WantBroadcast;
  BOOLEAN WantMulticast;
  BOOLEAN WantAllMulti;
  BOOLEAN WantTableMulticast;
  NDIS_STATUS TableStatus;

  if (!Adapter) {
    return;
  }

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_RX) == 0) {
    return;
  }

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VQ) == 0 || Adapter->CtrlVq.QueueSize == 0) {
    return;
  }

  if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
    return;
  }

  // Snapshot filter + multicast list under the adapter lock so the control
  // commands use a consistent view.
  NdisAcquireSpinLock(&Adapter->Lock);
  Filter = Adapter->PacketFilter;
  MulticastCount = Adapter->MulticastListSize;
  if (MulticastCount > NDIS_MAX_MULTICAST_LIST) {
    MulticastCount = NDIS_MAX_MULTICAST_LIST;
  }
  if (MulticastCount) {
    RtlCopyMemory(MulticastMacs, Adapter->MulticastList, MulticastCount * ETH_LENGTH_OF_ADDRESS);
  }
  RtlCopyMemory(UnicastMac, Adapter->CurrentMac, ETH_LENGTH_OF_ADDRESS);
  NdisReleaseSpinLock(&Adapter->Lock);

  // Best-effort: if called at DISPATCH_LEVEL, AerovNetCtrlSendCommand will
  // fail fast and we will keep relying on software filtering.
  WantPromisc = (Filter & NDIS_PACKET_TYPE_PROMISCUOUS) ? TRUE : FALSE;
  WantUnicast = WantPromisc ? TRUE : ((Filter & NDIS_PACKET_TYPE_DIRECTED) != 0);
  WantBroadcast = WantPromisc ? TRUE : ((Filter & NDIS_PACKET_TYPE_BROADCAST) != 0);
  WantMulticast = WantPromisc ? TRUE : ((Filter & (NDIS_PACKET_TYPE_MULTICAST | NDIS_PACKET_TYPE_ALL_MULTICAST)) != 0);

  On = WantPromisc ? 1u : 0u;
  (VOID)AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_RX, VIRTIO_NET_CTRL_RX_PROMISC, &On, sizeof(On), NULL);

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_RX_EXTRA) != 0) {
    // Explicitly program drop toggles for unicast/multicast/broadcast so device
    // models that implement virtio-net RX filtering behave consistently with the
    // NDIS packet filter.
    On = WantUnicast ? 0u : 1u;
    (VOID)AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_RX, VIRTIO_NET_CTRL_RX_NOUNI, &On, sizeof(On), NULL);

    On = WantMulticast ? 0u : 1u;
    (VOID)AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_RX, VIRTIO_NET_CTRL_RX_NOMULTI, &On, sizeof(On), NULL);

    On = WantBroadcast ? 0u : 1u;
    (VOID)AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_RX, VIRTIO_NET_CTRL_RX_NOBCAST, &On, sizeof(On), NULL);
  }

  TableStatus = NDIS_STATUS_SUCCESS;
  WantTableMulticast = (!WantPromisc && (Filter & NDIS_PACKET_TYPE_MULTICAST) != 0 && (Filter & NDIS_PACKET_TYPE_ALL_MULTICAST) == 0 &&
                        MulticastCount != 0)
                           ? TRUE
                           : FALSE;

  // Program the MAC filter tables (best-effort). Always provide a unicast entry
  // for the current MAC so directed traffic is received even when we fall back
  // to software filtering.
  if (WantTableMulticast) {
    TableStatus = AerovNetCtrlSetMacTable(Adapter, UnicastMac, MulticastCount, (const UCHAR*)MulticastMacs);
  } else {
    // Clear multicast table entries when not using selective multicast filtering.
    (VOID)AerovNetCtrlSetMacTable(Adapter, UnicastMac, 0, NULL);
  }

  if (WantPromisc) {
    WantAllMulti = TRUE;
  } else if (Filter & NDIS_PACKET_TYPE_ALL_MULTICAST) {
    WantAllMulti = TRUE;
  } else if (Filter & NDIS_PACKET_TYPE_MULTICAST) {
    if (MulticastCount == 0) {
      // Be conservative while Windows updates the multicast list: accept all
      // multicast frames until a list is installed.
      WantAllMulti = TRUE;
    } else if (WantTableMulticast && TableStatus == NDIS_STATUS_SUCCESS) {
      WantAllMulti = FALSE;
    } else {
      // Fall back to ALLMULTI so we don't miss multicast frames if MAC_TABLE_SET
      // fails for any reason.
      WantAllMulti = TRUE;
    }
  } else {
    WantAllMulti = FALSE;
  }

  On = WantAllMulti ? 1u : 0u;
  (VOID)AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_RX, VIRTIO_NET_CTRL_RX_ALLMULTI, &On, sizeof(On), NULL);
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
  USHORT NumQueues;
  USHORT CtrlQueueIndex;

  if (!Adapter || !Adapter->Vdev.CommonCfg || !Adapter->Vdev.DeviceCfg || !Adapter->Vdev.IsrStatus || !Adapter->Vdev.NotifyBase) {
    return NDIS_STATUS_FAILURE;
  }

  AerovNetCtrlVqRegistryInit(Adapter);

  /*
   * Contract v1 ring invariants (docs/windows7-virtio-driver-contract.md §2.3):
   * - MUST offer INDIRECT_DESC
   * - PACKED is not negotiated by the driver (split ring only)
   *
   * Aero contract v1 does not offer EVENT_IDX, but other hypervisors (notably
   * QEMU) may. Negotiate EVENT_IDX opportunistically when available to reduce
   * kicks/interrupts, while keeping the contract-v1 behaviour unchanged when it
   * is not offered.
   */
  Adapter->HostFeatures = VirtioPciReadDeviceFeatures(&Adapter->Vdev);

  // Contract v1 features (docs/windows7-virtio-driver-contract.md §3.2.3):
  // - required: VERSION_1 + INDIRECT_DESC + MAC + STATUS
  RequiredFeatures = VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS | AEROVNET_FEATURE_RING_INDIRECT_DESC;
  // Optional:
  // - EVENT_IDX: suppress kicks/interrupts when supported by the device.
  // - allow the device to report receive checksum status via virtio-net header
  //   flags (e.g. VIRTIO_NET_HDR_F_DATA_VALID).
  // - request the optional control virtqueue so we can issue runtime MAC/VLAN
  //   commands when supported (including RX mode toggles via CTRL_RX).
  // - MRG_RXBUF: allow a single received packet to span multiple buffers.
  //
  // Note: virtio-net uses VIRTIO_NET_F_GSO as a generic gate for the GSO fields
  // in `struct virtio_net_hdr` (gso_type/gso_size/hdr_len). Negotiate it
  // opportunistically so TSO/LSO works on implementations that require the bit
  // in addition to the per-protocol TSO feature bits (e.g.
  // VIRTIO_NET_F_HOST_TSO4/6).
  WantedFeatures = AEROVNET_FEATURE_RING_EVENT_IDX | VIRTIO_NET_F_CSUM | VIRTIO_NET_F_GUEST_CSUM | VIRTIO_NET_F_GSO |
                   VIRTIO_NET_F_HOST_TSO4 | VIRTIO_NET_F_HOST_TSO6 | VIRTIO_NET_F_HOST_ECN | VIRTIO_NET_F_CTRL_VQ |
                   VIRTIO_NET_F_CTRL_MAC_ADDR | VIRTIO_NET_F_CTRL_VLAN | VIRTIO_NET_F_CTRL_RX | VIRTIO_NET_F_CTRL_RX_EXTRA |
                   VIRTIO_NET_F_MRG_RXBUF;
  NegotiatedFeatures = 0;

  NtStatus = VirtioPciNegotiateFeatures(&Adapter->Vdev, RequiredFeatures, WantedFeatures, &NegotiatedFeatures);
  if (!NT_SUCCESS(NtStatus)) {
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Adapter->GuestFeatures = (UINT64)NegotiatedFeatures;
  AerovNetCtrlVqRegistryUpdate(Adapter);

  Adapter->RxHeaderBytes =
      (Adapter->GuestFeatures & VIRTIO_NET_F_MRG_RXBUF) ? (ULONG)sizeof(VIRTIO_NET_HDR_MRG_RXBUF) : (ULONG)sizeof(VIRTIO_NET_HDR);
  // RxHeaderBytes also determines the virtio-net header length used for TX
  // descriptor chains (the extra num_buffers field is unused on TX but is part
  // of the negotiated header layout).

  // Offload support depends on negotiated virtio-net features.
  Adapter->TxChecksumSupported = (Adapter->GuestFeatures & VIRTIO_NET_F_CSUM) ? TRUE : FALSE;
  Adapter->TxTsoV4Supported =
      (Adapter->TxChecksumSupported && (Adapter->GuestFeatures & VIRTIO_NET_F_GSO) && (Adapter->GuestFeatures & VIRTIO_NET_F_HOST_TSO4)) ? TRUE : FALSE;
  Adapter->TxTsoV6Supported =
      (Adapter->TxChecksumSupported && (Adapter->GuestFeatures & VIRTIO_NET_F_GSO) && (Adapter->GuestFeatures & VIRTIO_NET_F_HOST_TSO6)) ? TRUE : FALSE;

  // Enable all negotiated offloads by default; NDIS can toggle them via OID_TCP_OFFLOAD_PARAMETERS.
  Adapter->TxChecksumV4Enabled = Adapter->TxChecksumSupported;
  Adapter->TxChecksumV6Enabled = Adapter->TxChecksumSupported;
  Adapter->TxUdpChecksumV4Enabled = Adapter->TxChecksumSupported;
  Adapter->TxUdpChecksumV6Enabled = Adapter->TxChecksumSupported;
  Adapter->TxTsoV4Enabled = Adapter->TxTsoV4Supported;
  Adapter->TxTsoV6Enabled = Adapter->TxTsoV6Supported;
  Adapter->TxTsoMaxOffloadSize = 0x00010000u; // 64KiB total packet size.

  // Enable receive checksum indication by default when the device negotiated
  // VIRTIO_NET_F_GUEST_CSUM. NDIS can toggle it via OID_TCP_OFFLOAD_PARAMETERS.
  {
    const BOOLEAN RxCsum = (Adapter->GuestFeatures & VIRTIO_NET_F_GUEST_CSUM) ? TRUE : FALSE;
    Adapter->RxChecksumV4Enabled = RxCsum;
    Adapter->RxChecksumV6Enabled = RxCsum;
    Adapter->RxUdpChecksumV4Enabled = RxCsum;
    Adapter->RxUdpChecksumV6Enabled = RxCsum;
  }

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
  NumQueues = VirtioPciGetNumQueues(&Adapter->Vdev);
  if (NumQueues < 2) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return NDIS_STATUS_NOT_SUPPORTED;
  }

  Status = AerovNetProgramInterruptVectors(Adapter);
  if (Status != NDIS_STATUS_SUCCESS) {
    VirtioPciFailDevice(&Adapter->Vdev);
    VirtioPciResetDevice(&Adapter->Vdev);
    return Status;
  }

#if DBG
  if (Adapter->UseMsix) {
    DbgPrint("aero_virtio_net: interrupts: MSI messages=%hu all_on_vector0=%lu (config=%hu rx=%hu tx=%hu)\n",
             Adapter->MsixMessageCount,
             Adapter->MsixAllOnVector0 ? 1ul : 0ul,
             Adapter->MsixConfigVector,
             Adapter->MsixRxVector,
             Adapter->MsixTxVector);
  } else {
    DbgPrint("aero_virtio_net: interrupts: INTx\n");
  }
#endif

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

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VQ) != 0) {
    if (NumQueues < 3) {
      VirtioPciFailDevice(&Adapter->Vdev);
      VirtioPciResetDevice(&Adapter->Vdev);
      return NDIS_STATUS_NOT_SUPPORTED;
    }

    CtrlQueueIndex = (USHORT)(NumQueues - 1);
    Status = AerovNetSetupVq(Adapter, &Adapter->CtrlVq, CtrlQueueIndex, 0, 2);
    if (Status != NDIS_STATUS_SUCCESS) {
      VirtioPciFailDevice(&Adapter->Vdev);
      VirtioPciResetDevice(&Adapter->Vdev);
      return Status;
    }

    // The control virtqueue is used synchronously via polling; suppress
    // device->driver interrupts for this queue to avoid spurious DPC work when
    // the underlying transport routes all queues onto a shared interrupt.
    virtqueue_split_disable_interrupts(&Adapter->CtrlVq.Vq);
    if (Adapter->UseMsix) {
      // Also disable MSI-X routing for the control queue. Even though we set
      // VIRTQ_AVAIL_F_NO_INTERRUPT, being explicit avoids spurious interrupts on
      // devices/transports that ignore the suppression flag.
      (VOID)VirtioPciSetQueueMsixVector(&Adapter->Vdev, Adapter->CtrlVq.QueueIndex, VIRTIO_PCI_MSI_NO_VECTOR);
    }

    DbgPrint("virtio-net-ctrl-vq|INFO|init|queue_index=%hu|queue_size=%hu|features=0x%I64x\n", Adapter->CtrlVq.QueueIndex,
             Adapter->CtrlVq.QueueSize, (ULONGLONG)Adapter->GuestFeatures);
    AerovNetCtrlVqRegistryUpdate(Adapter);
  }

  // Allocate packet buffers.
  Adapter->Mtu = AEROVNET_MTU_DEFAULT;
  // Contract v1: allow up to 2 VLAN tags (QinQ), so the L2 header can be up to 22 bytes.
  Adapter->MaxFrameSize = Adapter->Mtu + 22;

  Adapter->RxBufferDataBytes = 2048;
  Adapter->RxBufferTotalBytes = Adapter->RxHeaderBytes + Adapter->RxBufferDataBytes;

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

  if ((Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VQ) != 0 && (Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_MAC_ADDR) != 0) {
    UCHAR Ack;
    Status = AerovNetCtrlSendCommand(Adapter, VIRTIO_NET_CTRL_MAC, VIRTIO_NET_CTRL_MAC_ADDR_SET, Adapter->CurrentMac, ETH_LENGTH_OF_ADDRESS, &Ack);
    DbgPrint("virtio-net-ctrl-vq|INFO|mac_addr_set|status=0x%08x|ack=%u\n", Status, Ack);
  }

  AerovNetCtrlVlanConfigureFromRegistry(Adapter);
  AerovNetCtrlUpdateRxMode(Adapter);

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
#if DBG
    NdisAcquireSpinLock(&Adapter->Lock);
    ASSERT(Adapter->OutstandingSgMappings == 0);
    NdisReleaseSpinLock(&Adapter->Lock);
#endif
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
      if (Adapter->DmaHandle && Nb) {
        NdisMFreeNetBufferSGList(Adapter->DmaHandle, TxReq->SgList, Nb);
      }
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

#if DBG
  DbgPrint("aero_virtio_net: tx cancel stats: before_sg=%ld after_sg=%ld after_submit=%ld\n",
           InterlockedCompareExchange(&g_AerovNetDbgTxCancelBeforeSg, 0, 0),
           InterlockedCompareExchange(&g_AerovNetDbgTxCancelAfterSg, 0, 0),
           InterlockedCompareExchange(&g_AerovNetDbgTxCancelAfterSubmit, 0, 0));
  DbgPrint("aero_virtio_net: tx csum stats: tcp_offload=%ld tcp_fallback=%ld udp_offload=%ld udp_fallback=%ld\n",
           InterlockedCompareExchange(&g_AerovNetDbgTxTcpCsumOffload, 0, 0),
           InterlockedCompareExchange(&g_AerovNetDbgTxTcpCsumFallback, 0, 0),
           InterlockedCompareExchange(&g_AerovNetDbgTxUdpCsumOffload, 0, 0),
           InterlockedCompareExchange(&g_AerovNetDbgTxUdpCsumFallback, 0, 0));
#endif

  AerovNetFreeTxResources(Adapter);
  AerovNetFreeRxResources(Adapter);
  AerovNetFreeCtrlPendingRequests(Adapter);

  AerovNetFreeVq(Adapter, &Adapter->RxVq);
  AerovNetFreeVq(Adapter, &Adapter->TxVq);
  AerovNetFreeVq(Adapter, &Adapter->CtrlVq);
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

  /*
   * NDIS uses TargetProcessors to select which CPU(s) should run the DPC.
   * This is an OUT parameter; always initialize it to a safe default even
   * when we return FALSE so NDIS never consumes stack garbage.
   *
   * 0 means "no preference" (NDIS chooses).
   */
  *TargetProcessors = 0;
  *QueueDefaultInterruptDpc = FALSE;

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
  InterlockedIncrement(&Adapter->InterruptCountByVector[0]);

  *QueueDefaultInterruptDpc = TRUE;
  return TRUE;
}

static VOID AerovNetInterruptDpcWork(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ BOOLEAN DoTx, _In_ BOOLEAN DoRx, _In_ BOOLEAN DoConfig) {
  LIST_ENTRY CompleteTxReqs;
  PNET_BUFFER_LIST CompleteNblHead;
  PNET_BUFFER_LIST CompleteNblTail;
  PNET_BUFFER_LIST IndicateHead;
  PNET_BUFFER_LIST IndicateTail;
  ULONG IndicateCount;
  BOOLEAN LinkChanged;
  BOOLEAN NewLinkUp;
  LONG TxDrained;
  LONG RxDrained;

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
  TxDrained = 0;
  RxDrained = 0;

  NdisAcquireSpinLock(&Adapter->Lock);

  if (Adapter->State == AerovNetAdapterStopped || Adapter->SurpriseRemoved) {
    NdisReleaseSpinLock(&Adapter->Lock);
    return;
  }

  if (DoTx || DoRx) {
    /*
     * Drain RX/TX queues while (best-effort) suppressing further interrupts.
     *
     * When EVENT_IDX is negotiated, the driver must update used_event to re-arm
     * interrupts; failing to do so can stall completions.
     */
    for (;;) {
      virtio_bool_t TxNeedsDrain;
      virtio_bool_t RxNeedsDrain;

      TxNeedsDrain = VIRTIO_FALSE;
      RxNeedsDrain = VIRTIO_FALSE;

      if (DoRx && Adapter->RxVq.QueueSize != 0) {
        virtqueue_split_disable_interrupts(&Adapter->RxVq.Vq);
      }
      if (DoTx && Adapter->TxVq.QueueSize != 0) {
        virtqueue_split_disable_interrupts(&Adapter->TxVq.Vq);
      }

      if (DoTx) {
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

          TxDrained++;

          TxReq = (AEROVNET_TX_REQUEST*)Cookie;

          if (TxReq) {
            if (TxReq->Nb) {
              Adapter->StatTxPackets++;
              Adapter->StatTxBytes += NET_BUFFER_DATA_LENGTH(TxReq->Nb);
            } else {
              Adapter->StatTxErrors++;
            }

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
      }

      if (DoRx) {
        // RX completions.
        for (;;) {
          const ULONG RxHdrBytes = Adapter->RxHeaderBytes;
          const BOOLEAN Mergeable = (Adapter->GuestFeatures & VIRTIO_NET_F_MRG_RXBUF) ? TRUE : FALSE;
          PVOID Cookie;
          uint32_t UsedLen;
          AEROVNET_RX_BUFFER* RxHead;
          AEROVNET_RX_BUFFER* RxTail;
          AEROVNET_RX_BUFFER* RxCur;
          USHORT NumBuffers;
          USHORT BufIndex;
          ULONG TotalPayloadLen;
          BOOLEAN Drop;
          BOOLEAN AbortRxDrain;

          Cookie = NULL;
          UsedLen = 0;

          if (Adapter->RxVq.QueueSize == 0) {
            break;
          }

          if (virtqueue_split_pop_used(&Adapter->RxVq.Vq, &Cookie, &UsedLen) == VIRTIO_FALSE) {
            break;
          }

          RxDrained++;

          RxHead = (AEROVNET_RX_BUFFER*)Cookie;
          if (!RxHead) {
            continue;
          }

          RxHead->PacketNext = NULL;
          RxHead->PacketBytes = 0;
          RxTail = RxHead;
          NumBuffers = 1;
          TotalPayloadLen = 0;
          Drop = FALSE;
          AbortRxDrain = FALSE;

          if (UsedLen < RxHdrBytes || UsedLen > RxHead->BufferBytes) {
            Adapter->StatRxErrors++;
            AerovNetResetRxBufferForReuse(Adapter, RxHead);
            InsertTailList(&Adapter->RxFreeList, &RxHead->Link);
            continue;
          }

          if (Mergeable) {
            const VIRTIO_NET_HDR_MRG_RXBUF* Hdr = (const VIRTIO_NET_HDR_MRG_RXBUF*)RxHead->BufferVa;
            NumBuffers = Hdr->NumBuffers;
            if (NumBuffers == 0 || NumBuffers > Adapter->RxVq.QueueSize) {
              Adapter->StatRxErrors++;
              AerovNetResetRxBufferForReuse(Adapter, RxHead);
              InsertTailList(&Adapter->RxFreeList, &RxHead->Link);
              continue;
            }
          }

          RxHead->PacketBytes = UsedLen - RxHdrBytes;
          TotalPayloadLen = RxHead->PacketBytes;

          // Pull remaining buffers for this packet if the device used more than one.
          for (BufIndex = 1; BufIndex < NumBuffers; BufIndex++) {
            PVOID Cookie2;
            uint32_t UsedLen2;
            AEROVNET_RX_BUFFER* Rx2;

            Cookie2 = NULL;
            UsedLen2 = 0;

            if (virtqueue_split_pop_used(&Adapter->RxVq.Vq, &Cookie2, &UsedLen2) == VIRTIO_FALSE) {
              Adapter->StatRxErrors++;
              AerovNetRecycleRxPacketLocked(Adapter, RxHead);
              // Cannot safely continue parsing the used ring without the full packet.
              AbortRxDrain = TRUE;
              break;
            }

            RxDrained++;

            Rx2 = (AEROVNET_RX_BUFFER*)Cookie2;
            if (!Rx2) {
              Adapter->StatRxErrors++;
              Drop = TRUE;
              continue;
            }

            Rx2->PacketNext = NULL;
            Rx2->PacketBytes = 0;
            RxTail->PacketNext = Rx2;
            RxTail = Rx2;

            if (UsedLen2 < RxHdrBytes || UsedLen2 > Rx2->BufferBytes) {
              Adapter->StatRxErrors++;
              Drop = TRUE;
              continue;
            }

            Rx2->PacketBytes = UsedLen2 - RxHdrBytes;
            TotalPayloadLen += Rx2->PacketBytes;
          }

          if (AbortRxDrain) {
            break;
          }

          // Contract v1: drop undersized/oversized Ethernet frames but always recycle.
          if (TotalPayloadLen < 14 || TotalPayloadLen > Adapter->MaxFrameSize) {
            Adapter->StatRxErrors++;
            Drop = TRUE;
          }

          if (!Drop && Adapter->State != AerovNetAdapterRunning) {
            Drop = TRUE;
          }

          // Packet filter / destination MAC check.
          if (!Drop) {
            if (RxHead->PacketBytes >= 14) {
              if (!AerovNetAcceptFrame(Adapter, RxHead->BufferVa + RxHdrBytes, TotalPayloadLen)) {
                Drop = TRUE;
              }
            } else {
              UCHAR EthHdr[14];
              ULONG Copied;

              RtlZeroMemory(EthHdr, sizeof(EthHdr));
              Copied = 0;
              for (RxCur = RxHead; RxCur && Copied < sizeof(EthHdr); RxCur = RxCur->PacketNext) {
                ULONG ToCopy = min(RxCur->PacketBytes, (ULONG)sizeof(EthHdr) - Copied);
                if (ToCopy) {
                  RtlCopyMemory(EthHdr + Copied, RxCur->BufferVa + RxHdrBytes, ToCopy);
                  Copied += ToCopy;
                }
              }

              if (Copied < sizeof(EthHdr)) {
                Adapter->StatRxErrors++;
                Drop = TRUE;
              } else if (!AerovNetAcceptFrame(Adapter, EthHdr, TotalPayloadLen)) {
                Drop = TRUE;
              }
            }
          }

          if (Drop) {
            AerovNetRecycleRxPacketLocked(Adapter, RxHead);
            continue;
          }

          // Chain payload MDLs and indicate a single NBL for the whole packet.
          for (RxCur = RxHead; RxCur; RxCur = RxCur->PacketNext) {
            RxCur->Indicated = TRUE;
            if (RxCur->Mdl) {
              RxCur->Mdl->ByteCount = RxCur->PacketBytes;
              RxCur->Mdl->Next = RxCur->PacketNext ? RxCur->PacketNext->Mdl : NULL;
            }
          }

          NET_BUFFER_CURRENT_MDL(RxHead->Nb) = RxHead->Mdl;
          NET_BUFFER_CURRENT_MDL_OFFSET(RxHead->Nb) = 0;
          NET_BUFFER_DATA_OFFSET(RxHead->Nb) = 0;
          NET_BUFFER_DATA_LENGTH(RxHead->Nb) = TotalPayloadLen;
          NET_BUFFER_LIST_STATUS(RxHead->Nbl) = NDIS_STATUS_SUCCESS;
          NET_BUFFER_LIST_NEXT_NBL(RxHead->Nbl) = NULL;
          NET_BUFFER_LIST_INFO(RxHead->Nbl, TcpIpChecksumNetBufferListInfo) = NULL;

          // Indicate RX checksum status (when negotiated) so Windows can skip
          // software checksum validation. When mergeable RX buffers are used,
          // the packet may be scattered across multiple MDLs; in that case, use
          // NdisGetDataBuffer to materialize a contiguous copy for header parsing.
          if (NumBuffers == 1) {
            AerovNetIndicateRxChecksum(Adapter, RxHead->Nbl, RxHead->BufferVa + RxHdrBytes, TotalPayloadLen,
                                       (const VIRTIO_NET_HDR*)RxHead->BufferVa);
          } else {
            VIRTIO_NET_HDR_OFFLOAD_RX_INFO RxInfo;
            BOOLEAN NeedCsumWork;
            PVOID FramePtr;

            // Avoid an expensive full-frame copy when checksum offload isn't
            // applicable (e.g. DATA_VALID not set, and no partial checksum
            // completion requested).
            NeedCsumWork = FALSE;
            (void)VirtioNetHdrOffloadParseRxHdr((const VIRTIO_NET_HDR*)RxHead->BufferVa, &RxInfo);
            if (RxInfo.NeedsCsum) {
              NeedCsumWork = TRUE;
            } else if (RxInfo.CsumValid) {
              if (Adapter->RxChecksumV4Enabled || Adapter->RxChecksumV6Enabled || Adapter->RxUdpChecksumV4Enabled ||
                  Adapter->RxUdpChecksumV6Enabled) {
                NeedCsumWork = TRUE;
              }
            }

            FramePtr = NULL;
            if (NeedCsumWork && RxHead->Nb && Adapter->RxChecksumScratch && TotalPayloadLen <= Adapter->RxChecksumScratchBytes) {
              FramePtr = NdisGetDataBuffer(RxHead->Nb, TotalPayloadLen, Adapter->RxChecksumScratch, 1, 0);
            }
            if (FramePtr) {
              AerovNetIndicateRxChecksum(Adapter, RxHead->Nbl, (const UCHAR*)FramePtr, TotalPayloadLen,
                                         (const VIRTIO_NET_HDR*)RxHead->BufferVa);
            }
          }

          if (IndicateTail) {
            NET_BUFFER_LIST_NEXT_NBL(IndicateTail) = RxHead->Nbl;
            IndicateTail = RxHead->Nbl;
          } else {
            IndicateHead = RxHead->Nbl;
            IndicateTail = RxHead->Nbl;
          }

          IndicateCount++;
          Adapter->StatRxPackets++;
          Adapter->StatRxBytes += TotalPayloadLen;
        }

        // Refill RX queue with any buffers we dropped.
        if (Adapter->State == AerovNetAdapterRunning) {
          AerovNetFillRxQueueLocked(Adapter);
        }
      }

      // Rearm interrupts and detect any completions that raced with re-arming.
      if (DoTx && Adapter->TxVq.QueueSize != 0) {
        TxNeedsDrain = virtqueue_split_enable_interrupts(&Adapter->TxVq.Vq);
      }
      if (DoRx && Adapter->RxVq.QueueSize != 0) {
        RxNeedsDrain = virtqueue_split_enable_interrupts(&Adapter->RxVq.Vq);
      }

      if ((DoTx == FALSE || TxNeedsDrain == VIRTIO_FALSE) && (DoRx == FALSE || RxNeedsDrain == VIRTIO_FALSE)) {
        break;
      }
    }
  }
  // Link state change handling (config interrupt).
  if (DoConfig) {
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

  if (TxDrained != 0) {
    InterlockedExchangeAdd(&Adapter->TxBuffersDrained, TxDrained);
  }
  if (RxDrained != 0) {
    InterlockedExchangeAdd(&Adapter->RxBuffersDrained, RxDrained);
  }

  // Free SG lists and return TX requests to free list.
  while (!IsListEmpty(&CompleteTxReqs)) {
    PLIST_ENTRY Entry = RemoveHeadList(&CompleteTxReqs);
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);

    if (TxReq->SgList) {
      if (Adapter->DmaHandle && TxReq->Nb) {
        NdisMFreeNetBufferSGList(Adapter->DmaHandle, TxReq->SgList, TxReq->Nb);
      }
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

static VOID AerovNetInterruptDpc(_In_ NDIS_HANDLE MiniportInterruptContext, _In_ PVOID MiniportDpcContext,
                                 _In_ PULONG NdisReserved1, _In_ PULONG NdisReserved2) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportInterruptContext;
  LONG Isr;
  BOOLEAN DoConfig;

  UNREFERENCED_PARAMETER(MiniportDpcContext);
  UNREFERENCED_PARAMETER(NdisReserved1);
  UNREFERENCED_PARAMETER(NdisReserved2);

  if (!Adapter) {
    return;
  }

  InterlockedIncrement(&Adapter->DpcCountByVector[0]);

  Isr = InterlockedExchange(&Adapter->IsrStatus, 0);
  DoConfig = ((Isr & 0x2) != 0) ? TRUE : FALSE;

  // Legacy INTx: keep existing behavior and service both queues on every interrupt.
  AerovNetInterruptDpcWork(Adapter, TRUE, TRUE, DoConfig);
}

static BOOLEAN AerovNetMessageInterruptIsr(_In_ NDIS_HANDLE MiniportInterruptContext,
                                          _In_ ULONG MessageId,
                                          _Out_ PBOOLEAN QueueDefaultInterruptDpc,
                                          _Out_ PULONG TargetProcessors) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportInterruptContext;

  /*
   * TargetProcessors is an OUT parameter (see AerovNetInterruptIsr for details).
   * Always initialize it so NDIS never observes an uninitialized value.
   */
  *TargetProcessors = 0;
  *QueueDefaultInterruptDpc = FALSE;

  if (!Adapter) {
    return FALSE;
  }

  if (Adapter->State == AerovNetAdapterStopped || Adapter->SurpriseRemoved) {
    return FALSE;
  }

  if (MessageId < AEROVNET_MSIX_MAX_MESSAGES) {
    InterlockedIncrement(&Adapter->InterruptCountByVector[MessageId]);
  }

  // MSI/MSI-X: do not touch the virtio ISR status register (INTx only). The
  // message ID indicates which MSI(-X) table entry fired.
  if (!Adapter->UseMsix) {
    // Defensive: if NDIS somehow calls the message ISR without MSI enabled,
    // treat it as ours and service everything in the DPC.
    *QueueDefaultInterruptDpc = TRUE;
    return TRUE;
  }

  if (Adapter->MsixAllOnVector0) {
    if (MessageId != (ULONG)Adapter->MsixConfigVector) {
      // Claim the interrupt to avoid spurious-unhandled MSI accounting, but do
      // not queue any DPC work for an unexpected message ID.
      return TRUE;
    }
    *QueueDefaultInterruptDpc = TRUE;
    return TRUE;
  }

  if (MessageId == (ULONG)Adapter->MsixConfigVector || MessageId == (ULONG)Adapter->MsixRxVector ||
      MessageId == (ULONG)Adapter->MsixTxVector) {
    *QueueDefaultInterruptDpc = TRUE;
    return TRUE;
  }

  // Not one of the vectors we programmed, but still an interrupt targeted at
  // this miniport. Claim it (no DPC) to avoid spurious MSI accounting.
  return TRUE;
}

static VOID AerovNetMessageInterruptDpc(_In_ NDIS_HANDLE MiniportInterruptContext,
                                       _In_ ULONG MessageId,
                                       _In_ PVOID MiniportDpcContext,
                                       _In_ PULONG NdisReserved1,
                                       _In_ PULONG NdisReserved2) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportInterruptContext;
  BOOLEAN DoTx;
  BOOLEAN DoRx;
  BOOLEAN DoConfig;

  UNREFERENCED_PARAMETER(MiniportDpcContext);
  UNREFERENCED_PARAMETER(NdisReserved1);
  UNREFERENCED_PARAMETER(NdisReserved2);

  if (!Adapter) {
    return;
  }

  if (MessageId < AEROVNET_MSIX_MAX_MESSAGES) {
    InterlockedIncrement(&Adapter->DpcCountByVector[MessageId]);
  }

  DoTx = FALSE;
  DoRx = FALSE;
  DoConfig = FALSE;

  if (!Adapter->UseMsix) {
    // Defensive: if NDIS somehow calls the message DPC without MSI enabled,
    // just service everything.
    DoTx = TRUE;
    DoRx = TRUE;
    DoConfig = TRUE;
  } else if (Adapter->MsixAllOnVector0) {
    if (MessageId != (ULONG)Adapter->MsixConfigVector) {
      return;
    }
    DoTx = TRUE;
    DoRx = TRUE;
    DoConfig = TRUE;
  } else {
    if (MessageId == (ULONG)Adapter->MsixConfigVector) {
      DoConfig = TRUE;
    } else if (MessageId == (ULONG)Adapter->MsixRxVector) {
      DoRx = TRUE;
    } else if (MessageId == (ULONG)Adapter->MsixTxVector) {
      DoTx = TRUE;
    } else {
      return;
    }
  }

  AerovNetInterruptDpcWork(Adapter, DoTx, DoRx, DoConfig);
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
  if (!TxReq) {
    return;
  }

  Adapter = TxReq->Adapter;
  if (!Adapter) {
    return;
  }

  ElemCount = ScatterGatherList ? ScatterGatherList->NumberOfElements : 0;
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
  } else if (Adapter->State == AerovNetAdapterStopped || Adapter->SurpriseRemoved) {
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_RESET_IN_PROGRESS, &CompleteHead, &CompleteTail);
    CompleteNow = TRUE;
  } else if (ScatterGatherList == NULL) {
    // NDIS can invoke the callback with a NULL SG list if DMA mapping fails
    // asynchronously. Treat this as a resources failure and complete the NET_BUFFER.
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_RESOURCES, &CompleteHead, &CompleteTail);
    CompleteNow = TRUE;
  } else if (ElemCount > AEROVNET_MAX_TX_SG_ELEMENTS) {
    AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_BUFFER_OVERFLOW, &CompleteHead, &CompleteTail);
    CompleteNow = TRUE;
  } else if (Adapter->State != AerovNetAdapterRunning) {
    // Paused: queue for later retry on restart.
    TxReq->State = AerovNetTxPendingSubmit;
    InsertTailList(&Adapter->TxPendingList, &TxReq->Link);
  } else {
    {
      if (!TxReq->HeaderBuilt) {
        NDIS_STATUS TxStatus = AerovNetBuildTxHeader(Adapter, TxReq);
        if (TxStatus != NDIS_STATUS_SUCCESS) {
          AerovNetCompleteTxRequest(Adapter, TxReq, TxStatus, &CompleteHead, &CompleteTail);
          CompleteNow = TRUE;
          goto ReleaseAndExit;
        }
        TxReq->HeaderBuilt = TRUE;
      }
    }

    Sg[0].addr = (uint64_t)TxReq->HeaderPa.QuadPart;
    Sg[0].len = (uint32_t)Adapter->RxHeaderBytes;
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

ReleaseAndExit:
  NdisReleaseSpinLock(&Adapter->Lock);

  if (CompleteNow) {
    // Free the SG list immediately; the device never saw the descriptors.
    if (ScatterGatherList && Adapter->DmaHandle && NbForFree) {
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
  NdisAcquireSpinLock(&Adapter->Lock);
  AerovNetSgMappingsDerefLocked(Adapter);
  NdisReleaseSpinLock(&Adapter->Lock);
}

static VOID AerovNetBuildNdisOffload(_In_ const AEROVNET_ADAPTER* Adapter, _In_ BOOLEAN UseCurrentConfig, _Out_ NDIS_OFFLOAD* Offload) {
  BOOLEAN TxTcp4;
  BOOLEAN TxUdp4;
  BOOLEAN TxTcp6;
  BOOLEAN TxUdp6;
  BOOLEAN RxTcp4;
  BOOLEAN RxUdp4;
  BOOLEAN RxTcp6;
  BOOLEAN RxUdp6;
  BOOLEAN TsoV4;
  BOOLEAN TsoV6;
  BOOLEAN RxSupported;

  if (!Offload) {
    return;
  }

  RtlZeroMemory(Offload, sizeof(*Offload));
  Offload->Header.Type = NDIS_OBJECT_TYPE_OFFLOAD;
  Offload->Header.Revision = NDIS_OFFLOAD_REVISION_1;
  Offload->Header.Size = (USHORT)sizeof(*Offload);

  // Start with negotiated (hardware) capabilities.
  TxTcp4 = Adapter->TxChecksumSupported;
  TxUdp4 = Adapter->TxChecksumSupported;
  TxTcp6 = Adapter->TxChecksumSupported;
  TxUdp6 = Adapter->TxChecksumSupported;

  RxSupported = ((Adapter->GuestFeatures & VIRTIO_NET_F_GUEST_CSUM) != 0) ? TRUE : FALSE;
  RxTcp4 = RxSupported;
  RxUdp4 = RxSupported;
  RxTcp6 = RxSupported;
  RxUdp6 = RxSupported;

  TsoV4 = Adapter->TxTsoV4Supported;
  TsoV6 = Adapter->TxTsoV6Supported;

  if (UseCurrentConfig) {
    // Reflect current enablement state (toggled via OID_TCP_OFFLOAD_PARAMETERS).
    TxTcp4 = TxTcp4 && Adapter->TxChecksumV4Enabled;
    TxUdp4 = TxUdp4 && Adapter->TxUdpChecksumV4Enabled;
    TxTcp6 = TxTcp6 && Adapter->TxChecksumV6Enabled;
    TxUdp6 = TxUdp6 && Adapter->TxUdpChecksumV6Enabled;
    RxTcp4 = RxTcp4 && Adapter->RxChecksumV4Enabled;
    RxUdp4 = RxUdp4 && Adapter->RxUdpChecksumV4Enabled;
    RxTcp6 = RxTcp6 && Adapter->RxChecksumV6Enabled;
    RxUdp6 = RxUdp6 && Adapter->RxUdpChecksumV6Enabled;

    // TSO implies TCP checksum offload.
    TsoV4 = TsoV4 && Adapter->TxTsoV4Enabled && Adapter->TxChecksumV4Enabled;
    TsoV6 = TsoV6 && Adapter->TxTsoV6Enabled && Adapter->TxChecksumV6Enabled;
  }

  // Only L4 checksum offload is supported. IPv4 header checksum is always computed in software.
  Offload->Checksum.IPv4Transmit.Encapsulation = NDIS_ENCAPSULATION_IEEE_802_3;
  Offload->Checksum.IPv4Transmit.IpOptionsSupported = (TxTcp4 || TxUdp4) ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Transmit.TcpOptionsSupported = TxTcp4 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Transmit.IpChecksum = NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Transmit.TcpChecksum = TxTcp4 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Transmit.UdpChecksum = TxUdp4 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;

  Offload->Checksum.IPv4Receive.Encapsulation = NDIS_ENCAPSULATION_IEEE_802_3;
  Offload->Checksum.IPv4Receive.IpOptionsSupported = NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Receive.TcpOptionsSupported = NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Receive.IpChecksum = NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Receive.TcpChecksum = RxTcp4 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv4Receive.UdpChecksum = RxUdp4 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;

  Offload->Checksum.IPv6Transmit.Encapsulation = NDIS_ENCAPSULATION_IEEE_802_3;
  Offload->Checksum.IPv6Transmit.IpExtensionHeadersSupported =
      (TxTcp6 || TxUdp6) ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv6Transmit.TcpOptionsSupported = TxTcp6 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv6Transmit.TcpChecksum = TxTcp6 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv6Transmit.UdpChecksum = TxUdp6 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;

  Offload->Checksum.IPv6Receive.Encapsulation = NDIS_ENCAPSULATION_IEEE_802_3;
  Offload->Checksum.IPv6Receive.IpExtensionHeadersSupported = NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv6Receive.TcpOptionsSupported = NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv6Receive.TcpChecksum = RxTcp6 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->Checksum.IPv6Receive.UdpChecksum = RxUdp6 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;

  // Large send offload v2 (TX only).
  Offload->LsoV2.IPv4.Encapsulation = NDIS_ENCAPSULATION_IEEE_802_3;
  Offload->LsoV2.IPv4.MaxOffLoadSize = TsoV4 ? Adapter->TxTsoMaxOffloadSize : 0;
  Offload->LsoV2.IPv4.MinSegmentCount = TsoV4 ? 2 : 0;
  Offload->LsoV2.IPv4.TcpOptionsSupported = TsoV4 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->LsoV2.IPv4.IpOptionsSupported = TsoV4 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;

  Offload->LsoV2.IPv6.Encapsulation = NDIS_ENCAPSULATION_IEEE_802_3;
  Offload->LsoV2.IPv6.MaxOffLoadSize = TsoV6 ? Adapter->TxTsoMaxOffloadSize : 0;
  Offload->LsoV2.IPv6.MinSegmentCount = TsoV6 ? 2 : 0;
  Offload->LsoV2.IPv6.TcpOptionsSupported = TsoV6 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
  Offload->LsoV2.IPv6.IpExtensionHeadersSupported = TsoV6 ? NDIS_OFFLOAD_SUPPORTED : NDIS_OFFLOAD_NOT_SUPPORTED;
}

static __forceinline BOOLEAN AerovNetOffloadParamTxEnabled(_In_ UCHAR V) {
  return (V == NDIS_OFFLOAD_PARAMETERS_TX_ENABLED_RX_DISABLED || V == NDIS_OFFLOAD_PARAMETERS_TX_RX_ENABLED);
}

static __forceinline BOOLEAN AerovNetOffloadParamRxEnabled(_In_ UCHAR V) {
  return (V == NDIS_OFFLOAD_PARAMETERS_RX_ENABLED_TX_DISABLED || V == NDIS_OFFLOAD_PARAMETERS_TX_RX_ENABLED);
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

    case OID_TCP_OFFLOAD_HARDWARE_CAPABILITIES:
    case OID_TCP_OFFLOAD_CURRENT_CONFIG: {
      NDIS_OFFLOAD Offload;
      BOOLEAN UseCurrent = (Oid == OID_TCP_OFFLOAD_CURRENT_CONFIG) ? TRUE : FALSE;

      // Serialize reads of the current enablement flags with OID set updates so
      // the returned config is internally consistent (best-effort).
      if (UseCurrent) {
        NdisAcquireSpinLock(&Adapter->Lock);
        AerovNetBuildNdisOffload(Adapter, TRUE, &Offload);
        NdisReleaseSpinLock(&Adapter->Lock);
      } else {
        AerovNetBuildNdisOffload(Adapter, FALSE, &Offload);
      }
      BytesNeeded = Offload.Header.Size;
      if (OutLen < BytesNeeded) {
        break;
      }
      RtlCopyMemory(OutBuffer, &Offload, BytesNeeded);
      BytesWritten = BytesNeeded;
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
    case OID_TCP_OFFLOAD_PARAMETERS: {
      const NDIS_OFFLOAD_PARAMETERS* Params;
      NDIS_STATUS SetStatus;

      BytesNeeded = sizeof(NDIS_OFFLOAD_PARAMETERS);
      if (InLen < BytesNeeded) {
        break;
      }

      Params = (const NDIS_OFFLOAD_PARAMETERS*)InBuffer;
      if (Params->Header.Type != NDIS_OBJECT_TYPE_DEFAULT || Params->Header.Revision != NDIS_OFFLOAD_PARAMETERS_REVISION_1 ||
          Params->Header.Size < sizeof(NDIS_OFFLOAD_PARAMETERS)) {
        return NDIS_STATUS_INVALID_DATA;
      }

      {
        BOOLEAN TxCsumV4;
        BOOLEAN TxCsumV6;
        BOOLEAN TxUdpCsumV4;
        BOOLEAN TxUdpCsumV6;
        BOOLEAN RxCsumV4;
        BOOLEAN RxCsumV6;
        BOOLEAN RxUdpCsumV4;
        BOOLEAN RxUdpCsumV6;
        BOOLEAN TxTsoV4;
        BOOLEAN TxTsoV6;
        const BOOLEAN RxSupported = ((Adapter->GuestFeatures & VIRTIO_NET_F_GUEST_CSUM) != 0) ? TRUE : FALSE;

        SetStatus = NDIS_STATUS_SUCCESS;

        // Serialize the full "read current -> apply deltas -> commit" update so
        // concurrent OID requests (including OID_TCP_OFFLOAD_CURRENT_CONFIG) see
        // a consistent config.
        NdisAcquireSpinLock(&Adapter->Lock);

        TxCsumV4 = Adapter->TxChecksumV4Enabled;
        TxCsumV6 = Adapter->TxChecksumV6Enabled;
        TxUdpCsumV4 = Adapter->TxUdpChecksumV4Enabled;
        TxUdpCsumV6 = Adapter->TxUdpChecksumV6Enabled;
        RxCsumV4 = Adapter->RxChecksumV4Enabled;
        RxCsumV6 = Adapter->RxChecksumV6Enabled;
        RxUdpCsumV4 = Adapter->RxUdpChecksumV4Enabled;
        RxUdpCsumV6 = Adapter->RxUdpChecksumV6Enabled;
        TxTsoV4 = Adapter->TxTsoV4Enabled;
        TxTsoV6 = Adapter->TxTsoV6Enabled;

        // NDIS_OFFLOAD_PARAMETERS fields are UCHAR enums:
        // 0 = no change, 1 = disabled, 2 = tx enabled, 3 = rx enabled, 4 = tx+rx enabled.
        {
          UCHAR V = Params->TCPIPv4Checksum;
          if (V != 0) {
            if (V < 1 || V > 4) {
              SetStatus = NDIS_STATUS_INVALID_DATA;
              goto TcpOffloadParamsDone;
            }
            TxCsumV4 = AerovNetOffloadParamTxEnabled(V);
            RxCsumV4 = AerovNetOffloadParamRxEnabled(V);
          }
        }

        {
          UCHAR V = Params->TCPIPv6Checksum;
          if (V != 0) {
            if (V < 1 || V > 4) {
              SetStatus = NDIS_STATUS_INVALID_DATA;
              goto TcpOffloadParamsDone;
            }
            TxCsumV6 = AerovNetOffloadParamTxEnabled(V);
            RxCsumV6 = AerovNetOffloadParamRxEnabled(V);
          }
        }

        {
          UCHAR V = Params->UDPIPv4Checksum;
          if (V != 0) {
            if (V < 1 || V > 4) {
              SetStatus = NDIS_STATUS_INVALID_DATA;
              goto TcpOffloadParamsDone;
            }
            TxUdpCsumV4 = AerovNetOffloadParamTxEnabled(V);
            RxUdpCsumV4 = AerovNetOffloadParamRxEnabled(V);
          }
        }

        {
          UCHAR V = Params->UDPIPv6Checksum;
          if (V != 0) {
            if (V < 1 || V > 4) {
              SetStatus = NDIS_STATUS_INVALID_DATA;
              goto TcpOffloadParamsDone;
            }
            TxUdpCsumV6 = AerovNetOffloadParamTxEnabled(V);
            RxUdpCsumV6 = AerovNetOffloadParamRxEnabled(V);
          }
        }

        {
          UCHAR V = Params->LsoV2IPv4;
          if (V != 0) {
            if (V < 1 || V > 4) {
              SetStatus = NDIS_STATUS_INVALID_DATA;
              goto TcpOffloadParamsDone;
            }
            TxTsoV4 = AerovNetOffloadParamTxEnabled(V);
          }
        }

        {
          UCHAR V = Params->LsoV2IPv6;
          if (V != 0) {
            if (V < 1 || V > 4) {
              SetStatus = NDIS_STATUS_INVALID_DATA;
              goto TcpOffloadParamsDone;
            }
            TxTsoV6 = AerovNetOffloadParamTxEnabled(V);
          }
        }

        // Clamp enablement by negotiated capabilities.
        if (!Adapter->TxChecksumSupported) {
          TxCsumV4 = FALSE;
          TxCsumV6 = FALSE;
          TxUdpCsumV4 = FALSE;
          TxUdpCsumV6 = FALSE;
        }
        if (!RxSupported) {
          RxCsumV4 = FALSE;
          RxCsumV6 = FALSE;
          RxUdpCsumV4 = FALSE;
          RxUdpCsumV6 = FALSE;
        }

        // TSO requires TCP checksum offload.
        if (!TxCsumV4) {
          TxTsoV4 = FALSE;
        }
        if (!TxCsumV6) {
          TxTsoV6 = FALSE;
        }
        if (!Adapter->TxTsoV4Supported) {
          TxTsoV4 = FALSE;
        }
        if (!Adapter->TxTsoV6Supported) {
          TxTsoV6 = FALSE;
        }

        Adapter->TxChecksumV4Enabled = TxCsumV4;
        Adapter->TxChecksumV6Enabled = TxCsumV6;
        Adapter->TxUdpChecksumV4Enabled = TxUdpCsumV4;
        Adapter->TxUdpChecksumV6Enabled = TxUdpCsumV6;
        Adapter->RxChecksumV4Enabled = RxCsumV4;
        Adapter->RxChecksumV6Enabled = RxCsumV6;
        Adapter->RxUdpChecksumV4Enabled = RxUdpCsumV4;
        Adapter->RxUdpChecksumV6Enabled = RxUdpCsumV6;
        Adapter->TxTsoV4Enabled = TxTsoV4;
        Adapter->TxTsoV6Enabled = TxTsoV6;

TcpOffloadParamsDone:
        NdisReleaseSpinLock(&Adapter->Lock);

        if (SetStatus != NDIS_STATUS_SUCCESS) {
          return SetStatus;
        }
      }

      BytesRead = sizeof(NDIS_OFFLOAD_PARAMETERS);
      break;
    }

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

      NdisAcquireSpinLock(&Adapter->Lock);
      Adapter->PacketFilter = Filter;
      NdisReleaseSpinLock(&Adapter->Lock);
      AerovNetCtrlUpdateRxMode(Adapter);
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

      NdisAcquireSpinLock(&Adapter->Lock);
      Adapter->MulticastListSize = Count;
      if (Count) {
        RtlCopyMemory(Adapter->MulticastList, InBuffer, InLen);
      }
      NdisReleaseSpinLock(&Adapter->Lock);

      AerovNetCtrlUpdateRxMode(Adapter);
      BytesRead = InLen;
      break;
    }

    case OID_802_3_CURRENT_ADDRESS: {
      const UCHAR* NewMac;
      NDIS_STATUS SetStatus;

      BytesNeeded = ETH_LENGTH_OF_ADDRESS;
      if (InLen < BytesNeeded) {
        break;
      }
      if (InLen != ETH_LENGTH_OF_ADDRESS) {
        return NDIS_STATUS_INVALID_LENGTH;
      }

      NewMac = (const UCHAR*)InBuffer;
      if ((NewMac[0] & 0x01u) != 0 || AerovNetIsBroadcastAddress(NewMac)) {
        return NDIS_STATUS_INVALID_DATA;
      }

      if (AerovNetMacEqual(Adapter->CurrentMac, NewMac)) {
        BytesRead = ETH_LENGTH_OF_ADDRESS;
        break;
      }

      SetStatus = AerovNetCtrlSetMac(Adapter, NewMac);
      if (SetStatus != NDIS_STATUS_SUCCESS) {
        return SetStatus;
      }

      NdisAcquireSpinLock(&Adapter->Lock);
      RtlCopyMemory(Adapter->CurrentMac, NewMac, ETH_LENGTH_OF_ADDRESS);
      NdisReleaseSpinLock(&Adapter->Lock);
      AerovNetCtrlUpdateRxMode(Adapter);
      BytesRead = ETH_LENGTH_OF_ADDRESS;
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
  if (Adapter->State == AerovNetAdapterStopped || Adapter->SurpriseRemoved) {
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

    for (Nb = NET_BUFFER_LIST_FIRST_NB(Nbl); Nb;) {
      PNET_BUFFER NextNb = NET_BUFFER_NEXT_NB(Nb);
      AEROVNET_TX_REQUEST* TxReq;
      NDIS_STATUS SgStatus;

      TxReq = NULL;

      NdisAcquireSpinLock(&Adapter->Lock);

      if (Adapter->State != AerovNetAdapterRunning || Adapter->SurpriseRemoved) {
        NDIS_STATUS TxStatus = (Adapter->State == AerovNetAdapterPaused && !Adapter->SurpriseRemoved) ? NDIS_STATUS_PAUSED : NDIS_STATUS_RESET_IN_PROGRESS;
        AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, TxStatus, &CompleteHead, &CompleteTail);
        NdisReleaseSpinLock(&Adapter->Lock);
        Nb = NextNb;
        continue;
      }

      // Contract v1 frame size rules:
      // - Without TSO/LSO, drop undersized/oversized frames (<= 1522, incl. VLAN).
      // - With negotiated + enabled TSO, allow larger packets when NDIS requests LSO.
      //
      // For plain Ethernet frames we complete successfully (no delivery guarantee).
      {
        ULONG FrameLen = NET_BUFFER_DATA_LENGTH(Nb);
        ULONG MaxLen = 1522;
        BOOLEAN WantsLso = (NET_BUFFER_LIST_INFO(Nbl, TcpLargeSendNetBufferListInfo) != NULL) ? TRUE : FALSE;

        if (WantsLso && (Adapter->TxTsoV4Enabled || Adapter->TxTsoV6Enabled)) {
          MaxLen = Adapter->TxTsoMaxOffloadSize;
        }

        if (FrameLen < 14) {
          Adapter->StatTxErrors++;
          AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, NDIS_STATUS_SUCCESS, &CompleteHead, &CompleteTail);
          NdisReleaseSpinLock(&Adapter->Lock);
          Nb = NextNb;
          continue;
        }

        if (FrameLen > MaxLen) {
          Adapter->StatTxErrors++;
          if (WantsLso) {
            AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, NDIS_STATUS_INVALID_PACKET, &CompleteHead, &CompleteTail);
          } else {
            AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, NDIS_STATUS_SUCCESS, &CompleteHead, &CompleteTail);
          }
          NdisReleaseSpinLock(&Adapter->Lock);
          Nb = NextNb;
          continue;
        }
      }

      if (IsListEmpty(&Adapter->TxFreeList)) {
        AerovNetTxNblCompleteOneNetBufferLocked(Adapter, Nbl, NDIS_STATUS_RESOURCES, &CompleteHead, &CompleteTail);
        NdisReleaseSpinLock(&Adapter->Lock);
        Nb = NextNb;
        continue;
      }

      {
        PLIST_ENTRY Entry = RemoveHeadList(&Adapter->TxFreeList);
        TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);
      }

      TxReq->State = AerovNetTxAwaitingSg;
      TxReq->Cancelled = FALSE;
      TxReq->HeaderBuilt = FALSE;
      TxReq->Adapter = Adapter;
      // Snapshot offload enablement at accept time so queued/pending sends do not
      // consult live adapter config (which can change via OID).
      TxReq->TxChecksumV4Enabled = Adapter->TxChecksumV4Enabled;
      TxReq->TxChecksumV6Enabled = Adapter->TxChecksumV6Enabled;
      TxReq->TxUdpChecksumV4Enabled = Adapter->TxUdpChecksumV4Enabled;
      TxReq->TxUdpChecksumV6Enabled = Adapter->TxUdpChecksumV6Enabled;
      TxReq->TxTsoV4Enabled = Adapter->TxTsoV4Enabled;
      TxReq->TxTsoV6Enabled = Adapter->TxTsoV6Enabled;
      TxReq->Nbl = Nbl;
      TxReq->Nb = Nb;
      TxReq->SgList = NULL;
      InsertTailList(&Adapter->TxAwaitingSgList, &TxReq->Link);

      AerovNetSgMappingsRefLocked(Adapter);

      NdisReleaseSpinLock(&Adapter->Lock);

      SgStatus = NdisMAllocateNetBufferSGList(Adapter->DmaHandle, Nb, TxReq, 0);
      if (SgStatus != NDIS_STATUS_SUCCESS && SgStatus != NDIS_STATUS_PENDING) {
        // SG allocation failed synchronously; undo the TxReq.
        NdisAcquireSpinLock(&Adapter->Lock);
        if (TxReq->State == AerovNetTxAwaitingSg) {
          RemoveEntryList(&TxReq->Link);
        }
        AerovNetCompleteTxRequest(Adapter, TxReq, SgStatus, &CompleteHead, &CompleteTail);
        AerovNetFreeTxRequestNoLock(Adapter, TxReq);
        AerovNetSgMappingsDerefLocked(Adapter);
        NdisReleaseSpinLock(&Adapter->Lock);
      }

      Nb = NextNb;
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
    AerovNetRecycleRxPacketLocked(Adapter, Rx);
  }

  if (Adapter->State == AerovNetAdapterRunning && !Adapter->SurpriseRemoved) {
    AerovNetFillRxQueueLocked(Adapter);
  }

  NdisReleaseSpinLock(&Adapter->Lock);
}

static VOID AerovNetMiniportCancelSend(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PVOID CancelId) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
  PLIST_ENTRY Entry;
  PNET_BUFFER_LIST CompleteHead;
  PNET_BUFFER_LIST CompleteTail;

  if (!Adapter) {
    return;
  }

  CompleteHead = NULL;
  CompleteTail = NULL;

  NdisAcquireSpinLock(&Adapter->Lock);
  if (Adapter->State == AerovNetAdapterStopped) {
    NdisReleaseSpinLock(&Adapter->Lock);
    return;
  }

  // Mark any requests still awaiting SG mapping as cancelled; they will be
  // completed in the SG callback once the mapping finishes.
  for (Entry = Adapter->TxAwaitingSgList.Flink; Entry != &Adapter->TxAwaitingSgList; Entry = Entry->Flink) {
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);
    if (TxReq->Nbl != NULL && NET_BUFFER_LIST_CANCEL_ID(TxReq->Nbl) == CancelId) {
      if (!TxReq->Cancelled) {
        TxReq->Cancelled = TRUE;
#if DBG
        InterlockedIncrement(&g_AerovNetDbgTxCancelBeforeSg);
#endif
      }
    }
  }

  // Cancel requests queued pending submission (SG mapping already complete).
  Entry = Adapter->TxPendingList.Flink;
  while (Entry != &Adapter->TxPendingList) {
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);
    Entry = Entry->Flink;

    if (TxReq->Nbl != NULL && NET_BUFFER_LIST_CANCEL_ID(TxReq->Nbl) == CancelId) {
      PSCATTER_GATHER_LIST SgList = TxReq->SgList;
      PNET_BUFFER Nb = TxReq->Nb;
      NDIS_HANDLE DmaHandle = Adapter->DmaHandle;

      RemoveEntryList(&TxReq->Link);

      // Free the SG list while the NET_BUFFER is still owned by the miniport.
      // This avoids races with HaltEx completing the NBL before we get a chance to
      // return here and free the mapping.
      TxReq->SgList = NULL;
      if (SgList && DmaHandle && Nb) {
        NdisMFreeNetBufferSGList(DmaHandle, SgList, Nb);
      }

      AerovNetCompleteTxRequest(Adapter, TxReq, NDIS_STATUS_REQUEST_ABORTED, &CompleteHead, &CompleteTail);
#if DBG
      InterlockedIncrement(&g_AerovNetDbgTxCancelAfterSg);
#endif

      AerovNetFreeTxRequestNoLock(Adapter, TxReq);
    }
  }

  // Requests already submitted to the device cannot be cancelled deterministically;
  // track them for debugging/diagnostics only.
#if DBG
  for (Entry = Adapter->TxSubmittedList.Flink; Entry != &Adapter->TxSubmittedList; Entry = Entry->Flink) {
    AEROVNET_TX_REQUEST* TxReq = CONTAINING_RECORD(Entry, AEROVNET_TX_REQUEST, Link);
    if (TxReq->Nbl != NULL && NET_BUFFER_LIST_CANCEL_ID(TxReq->Nbl) == CancelId) {
      if (!TxReq->Cancelled) {
        TxReq->Cancelled = TRUE;
        InterlockedIncrement(&g_AerovNetDbgTxCancelAfterSubmit);
      }
    }
  }
#endif

  NdisReleaseSpinLock(&Adapter->Lock);

  while (CompleteHead) {
    PNET_BUFFER_LIST Nbl = CompleteHead;
    CompleteHead = NET_BUFFER_LIST_NEXT_NBL(Nbl);
    NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;
    AerovNetCompleteNblSend(Adapter, Nbl, NET_BUFFER_LIST_STATUS(Nbl));
  }
}

static VOID AerovNetMiniportDevicePnPEventNotify(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNET_DEVICE_PNP_EVENT NetDevicePnPEvent) {
  AEROVNET_ADAPTER* Adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
  NDIS_HANDLE InterruptHandle;
  ULONG I;
  const BOOLEAN CanDeregisterInterrupt = (KeGetCurrentIrql() == PASSIVE_LEVEL) ? TRUE : FALSE;

  if (!Adapter || !NetDevicePnPEvent) {
    return;
  }

  if (NetDevicePnPEvent->DevicePnPEvent == NdisDevicePnPEventSurpriseRemoved) {
    // Set this flag first without taking the adapter lock. The surprise removal
    // callback can race with DPC/ISR contexts; setting the flag early allows
    // other paths to quickly stop issuing virtio BAR MMIO (e.g. queue notify).
    Adapter->SurpriseRemoved = TRUE;

    // Best-effort: immediately invalidate BAR-backed virtio pointers so any
    // concurrent notify/config path becomes a no-op even if it already passed a
    // SurpriseRemoved check before this flag was set.
    //
    // Invalidate NotifyBase/CommonCfg first so VirtioPciNotifyQueue() cannot fall
    // back to computing notify addresses via MMIO once the cache pointer is cleared.
    //
    // These are one-way transitions (non-NULL -> NULL) and are safe to perform
    // without holding Adapter->Lock.
    (VOID)InterlockedExchangePointer((PVOID*)&Adapter->Vdev.NotifyBase, NULL);
    (VOID)InterlockedExchangePointer((PVOID*)&Adapter->Vdev.CommonCfg, NULL);
    (VOID)InterlockedExchangePointer((PVOID*)&Adapter->Vdev.IsrStatus, NULL);
    (VOID)InterlockedExchangePointer((PVOID*)&Adapter->Vdev.DeviceCfg, NULL);
    (VOID)InterlockedExchangePointer((PVOID*)&Adapter->Vdev.QueueNotifyAddrCache, NULL);
    Adapter->Vdev.QueueNotifyAddrCacheCount = 0;
    for (I = 0; I < RTL_NUMBER_OF(Adapter->QueueNotifyAddrCache); I++) {
      (VOID)InterlockedExchangePointer((PVOID*)&Adapter->QueueNotifyAddrCache[I], NULL);
    }

    NdisAcquireSpinLock(&Adapter->Lock);
    Adapter->State = AerovNetAdapterStopped;
    InterruptHandle = NULL;
    if (CanDeregisterInterrupt) {
      InterruptHandle = Adapter->InterruptHandle;
      Adapter->InterruptHandle = NULL;
    }

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

    // Drain and stop interrupt processing as early as possible during surprise removal.
    // This ensures no ISR/DPC path will attempt BAR MMIO after the device disappears.
    if (InterruptHandle) {
      NdisMDeregisterInterruptEx(InterruptHandle);
    }

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

  // Ensure no NDIS SG mapping callbacks are still in flight when Pause returns.
  // This avoids a race where Pause/Restart is complete but a late SG callback
  // still tries to enqueue/complete a TX request.
  if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
    (VOID)KeWaitForSingleObject(&Adapter->OutstandingSgEvent, Executive, KernelMode, FALSE, NULL);
#if DBG
    NdisAcquireSpinLock(&Adapter->Lock);
    ASSERT(Adapter->OutstandingSgMappings == 0);
    NdisReleaseSpinLock(&Adapter->Lock);
#endif
  }

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
      if (Adapter->DmaHandle && Nb) {
        NdisMFreeNetBufferSGList(Adapter->DmaHandle, TxReq->SgList, Nb);
      }
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
  NDIS_HANDLE InterruptHandle;

  UNREFERENCED_PARAMETER(HaltAction);

  if (!Adapter) {
    return;
  }

  NdisAcquireSpinLock(&Adapter->Lock);
  Adapter->State = AerovNetAdapterStopped;
  InterruptHandle = Adapter->InterruptHandle;
  Adapter->InterruptHandle = NULL;
  NdisReleaseSpinLock(&Adapter->Lock);

  // Ensure no ISR/DPC is still running before we start tearing down virtqueues and
  // TX request storage. NDIS can still have a DPC in-flight even after the
  // adapter state transitions to stopped.
  if (InterruptHandle) {
    NdisMDeregisterInterruptEx(InterruptHandle);
  }
  AerovNetDiagDetachAdapter(Adapter);
  AerovNetVirtioStop(Adapter);

  AerovNetCleanupAdapter(Adapter);
}

static NDIS_STATUS AerovNetMiniportInitializeEx(_In_ NDIS_HANDLE MiniportAdapterHandle, _In_ NDIS_HANDLE MiniportDriverContext,
                                                _In_ PNDIS_MINIPORT_INIT_PARAMETERS MiniportInitParameters) {
  NDIS_STATUS Status;
  AEROVNET_ADAPTER* Adapter;
  NDIS_MINIPORT_ADAPTER_REGISTRATION_ATTRIBUTES Reg;
  NDIS_MINIPORT_ADAPTER_OFFLOAD_ATTRIBUTES OffAttr;
  NDIS_OFFLOAD OffloadCaps;
  NDIS_OFFLOAD OffloadConfig;
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
  Adapter->DiagRefCount = 0;

  virtio_os_ndis_get_ops(&Adapter->VirtioOps);
  Adapter->VirtioOpsCtx.pool_tag = AEROVNET_TAG;

  NdisAllocateSpinLock(&Adapter->Lock);
  KeInitializeEvent(&Adapter->OutstandingSgEvent, NotificationEvent, TRUE);
  KeInitializeEvent(&Adapter->DiagRefEvent, NotificationEvent, TRUE);
  KeInitializeEvent(&Adapter->CtrlCmdEvent, SynchronizationEvent, TRUE);

  InitializeListHead(&Adapter->RxFreeList);
  InitializeListHead(&Adapter->TxFreeList);
  InitializeListHead(&Adapter->TxAwaitingSgList);
  InitializeListHead(&Adapter->TxPendingList);
  InitializeListHead(&Adapter->TxSubmittedList);
  InitializeListHead(&Adapter->CtrlPendingList);

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

  // Interrupt registration (MSI/MSI-X opt-in via INF, INTx fallback).
  RtlZeroMemory(&Intr, sizeof(Intr));
  Intr.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_INTERRUPT;
  Intr.Header.Revision = NDIS_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_2;
#ifdef NDIS_SIZEOF_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_2
  Intr.Header.Size = NDIS_SIZEOF_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_2;
#else
  Intr.Header.Size = sizeof(Intr);
#endif
  Intr.InterruptHandler = AerovNetInterruptIsr;
  Intr.InterruptDpcHandler = AerovNetInterruptDpc;
  Intr.MessageInterruptHandler = AerovNetMessageInterruptIsr;
  Intr.MessageInterruptDpcHandler = AerovNetMessageInterruptDpc;

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

  RtlZeroMemory(&OffloadCaps, sizeof(OffloadCaps));
  RtlZeroMemory(&OffloadConfig, sizeof(OffloadConfig));
  AerovNetBuildNdisOffload(Adapter, FALSE, &OffloadCaps);
  AerovNetBuildNdisOffload(Adapter, TRUE, &OffloadConfig);

  RtlZeroMemory(&OffAttr, sizeof(OffAttr));
  OffAttr.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_ADAPTER_OFFLOAD_ATTRIBUTES;
  OffAttr.Header.Revision = NDIS_MINIPORT_ADAPTER_OFFLOAD_ATTRIBUTES_REVISION_1;
  OffAttr.Header.Size = sizeof(OffAttr);
  OffAttr.DefaultOffloadConfiguration = &OffloadConfig;
  OffAttr.HardwareOffloadCapabilities = &OffloadCaps;

  Status = NdisMSetMiniportAttributes(MiniportAdapterHandle, (PNDIS_MINIPORT_ADAPTER_ATTRIBUTES)&OffAttr);
  if (Status != NDIS_STATUS_SUCCESS) {
    // Fail safe: if NDIS rejects the offload advertisement, disable offloads
    // entirely so the upper stack does not send packets that rely on hardware
    // assistance.
    Adapter->TxChecksumSupported = FALSE;
    Adapter->TxTsoV4Supported = FALSE;
    Adapter->TxTsoV6Supported = FALSE;
    Adapter->TxChecksumV4Enabled = FALSE;
    Adapter->TxChecksumV6Enabled = FALSE;
    Adapter->TxUdpChecksumV4Enabled = FALSE;
    Adapter->TxUdpChecksumV6Enabled = FALSE;
    Adapter->TxTsoV4Enabled = FALSE;
    Adapter->TxTsoV6Enabled = FALSE;
    Adapter->RxChecksumV4Enabled = FALSE;
    Adapter->RxChecksumV6Enabled = FALSE;
    Adapter->RxUdpChecksumV4Enabled = FALSE;
    Adapter->RxUdpChecksumV6Enabled = FALSE;
    Adapter->TxTsoMaxOffloadSize = 0;
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

  AerovNetDiagAttachAdapter(Adapter);
  AerovNetIndicateLinkState(Adapter);

  return NDIS_STATUS_SUCCESS;
}

static __forceinline AEROVNET_ADAPTER* AerovNetDiagReferenceAdapter(VOID) {
  AEROVNET_ADAPTER* Adapter;
  LONG Ref;

  if (!g_DiagLockInitialized) {
    return NULL;
  }

  Adapter = NULL;
  NdisAcquireSpinLock(&g_DiagLock);
  Adapter = g_DiagAdapter;
  if (Adapter != NULL) {
    Ref = InterlockedIncrement(&Adapter->DiagRefCount);
    if (Ref == 1) {
      (VOID)KeResetEvent(&Adapter->DiagRefEvent);
    }
  }
  NdisReleaseSpinLock(&g_DiagLock);
  return Adapter;
}

static __forceinline VOID AerovNetDiagDereferenceAdapter(_Inout_ AEROVNET_ADAPTER* Adapter) {
  LONG Ref;

  if (!Adapter) {
    return;
  }

  Ref = InterlockedDecrement(&Adapter->DiagRefCount);
  if (Ref == 0) {
    KeSetEvent(&Adapter->DiagRefEvent, IO_NO_INCREMENT, FALSE);
  }
}

static VOID AerovNetDiagAttachAdapter(_In_ AEROVNET_ADAPTER* Adapter) {
  if (!Adapter || !g_DiagLockInitialized) {
    return;
  }

  NdisAcquireSpinLock(&g_DiagLock);
  g_DiagAdapter = Adapter;
  NdisReleaseSpinLock(&g_DiagLock);
}

static VOID AerovNetDiagDetachAdapter(_In_ AEROVNET_ADAPTER* Adapter) {
  if (!Adapter || !g_DiagLockInitialized) {
    return;
  }

  NdisAcquireSpinLock(&g_DiagLock);
  if (g_DiagAdapter == Adapter) {
    g_DiagAdapter = NULL;
  }
  NdisReleaseSpinLock(&g_DiagLock);

  // HaltEx is expected to run at PASSIVE_LEVEL; wait for outstanding diagnostic IOCTLs so we don't unmap BAR0 while
  // a user-mode query is reading virtio registers.
  if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
    (VOID)KeWaitForSingleObject(&Adapter->DiagRefEvent, Executive, KernelMode, FALSE, NULL);
  }
}

static NTSTATUS AerovNetDiagDispatchDefault(_In_ PDEVICE_OBJECT DeviceObject, _Inout_ PIRP Irp) {
  UNREFERENCED_PARAMETER(DeviceObject);

  Irp->IoStatus.Status = STATUS_INVALID_DEVICE_REQUEST;
  Irp->IoStatus.Information = 0;
  IoCompleteRequest(Irp, IO_NO_INCREMENT);
  return STATUS_INVALID_DEVICE_REQUEST;
}

static NTSTATUS AerovNetDiagDispatchCreateClose(_In_ PDEVICE_OBJECT DeviceObject, _Inout_ PIRP Irp) {
  UNREFERENCED_PARAMETER(DeviceObject);

  Irp->IoStatus.Status = STATUS_SUCCESS;
  Irp->IoStatus.Information = 0;
  IoCompleteRequest(Irp, IO_NO_INCREMENT);
  return STATUS_SUCCESS;
}

static NTSTATUS AerovNetDiagDispatchDeviceControl(_In_ PDEVICE_OBJECT DeviceObject, _Inout_ PIRP Irp) {
  PIO_STACK_LOCATION IrpSp;
  ULONG Ioctl;
  ULONG OutLen;
  NTSTATUS Status;
  AEROVNET_ADAPTER* Adapter;
  AEROVNET_DIAG_INFO Info;
  AEROVNET_OFFLOAD_STATS OffloadStats;
  ULONG CopyLen;
  volatile virtio_pci_common_cfg* CommonCfg;

  UNREFERENCED_PARAMETER(DeviceObject);

  IrpSp = IoGetCurrentIrpStackLocation(Irp);
  Ioctl = IrpSp->Parameters.DeviceIoControl.IoControlCode;
  OutLen = IrpSp->Parameters.DeviceIoControl.OutputBufferLength;

  Status = STATUS_INVALID_DEVICE_REQUEST;
  CopyLen = 0;
  CommonCfg = NULL;

  if (Ioctl != AEROVNET_DIAG_IOCTL_QUERY && Ioctl != AEROVNET_IOCTL_QUERY_OFFLOAD_STATS) {
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = 0;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
  }

  if (Irp->AssociatedIrp.SystemBuffer == NULL || OutLen < sizeof(ULONG) * 2) {
    Status = STATUS_BUFFER_TOO_SMALL;
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = 0;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
  }

  if (Ioctl == AEROVNET_IOCTL_QUERY_OFFLOAD_STATS && OutLen < sizeof(OffloadStats)) {
    Status = STATUS_BUFFER_TOO_SMALL;
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = 0;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
  }

  Adapter = AerovNetDiagReferenceAdapter();
  if (Adapter == NULL) {
    Status = STATUS_DEVICE_NOT_READY;
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = 0;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
  }

  // Snapshot cached state under the adapter lock.
  NdisAcquireSpinLock(&Adapter->Lock);

  if (Ioctl == AEROVNET_DIAG_IOCTL_QUERY) {
    RtlZeroMemory(&Info, sizeof(Info));
    Info.Version = AEROVNET_DIAG_INFO_VERSION;
    Info.Size = sizeof(Info);
    Info.MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
    Info.MsixRxVector = VIRTIO_PCI_MSI_NO_VECTOR;
    Info.MsixTxVector = VIRTIO_PCI_MSI_NO_VECTOR;

    Info.HostFeatures = Adapter->HostFeatures;
    Info.GuestFeatures = Adapter->GuestFeatures;

    Info.InterruptMode = Adapter->UseMsix ? AEROVNET_INTERRUPT_MODE_MSI : AEROVNET_INTERRUPT_MODE_INTX;
    // `MessageCount` reflects how many message interrupts Windows granted, not
    // necessarily whether the driver ended up using MSI-X (we can fall back to
    // INTx if vector programming fails).
    Info.MessageCount = (ULONG)Adapter->MsixMessageCount;
    Info.MsixConfigVector = Adapter->MsixConfigVector;
    Info.MsixRxVector = Adapter->MsixRxVector;
    Info.MsixTxVector = Adapter->MsixTxVector;
    CommonCfg = (Adapter->State != AerovNetAdapterStopped && !Adapter->SurpriseRemoved) ? Adapter->Vdev.CommonCfg : NULL;

    if (Adapter->UseMsix) {
      Info.Flags |= AEROVNET_DIAG_FLAG_USE_MSIX;
      if (Adapter->MsixAllOnVector0) {
        Info.Flags |= AEROVNET_DIAG_FLAG_MSIX_ALL_ON_VECTOR0;
      }
    }
    if (Adapter->MsixVectorProgrammingFailed) {
      Info.Flags |= AEROVNET_DIAG_FLAG_MSIX_VECTOR_PROGRAMMING_FAILED;
    }

    if (Adapter->SurpriseRemoved) {
      Info.Flags |= AEROVNET_DIAG_FLAG_SURPRISE_REMOVED;
    }

    if (Adapter->State == AerovNetAdapterRunning) {
      Info.Flags |= AEROVNET_DIAG_FLAG_ADAPTER_RUNNING;
    } else if (Adapter->State == AerovNetAdapterPaused) {
      Info.Flags |= AEROVNET_DIAG_FLAG_ADAPTER_PAUSED;
    }

    Info.RxQueueSize = Adapter->RxVq.QueueSize;
    Info.TxQueueSize = Adapter->TxVq.QueueSize;

    Info.RxAvailIdx = (USHORT)Adapter->RxVq.Vq.avail_idx;
    Info.RxUsedIdx = (Adapter->RxVq.Vq.used != NULL) ? (USHORT)Adapter->RxVq.Vq.used->idx : 0;
    Info.TxAvailIdx = (USHORT)Adapter->TxVq.Vq.avail_idx;
    Info.TxUsedIdx = (Adapter->TxVq.Vq.used != NULL) ? (USHORT)Adapter->TxVq.Vq.used->idx : 0;

    Info.TxChecksumSupported = Adapter->TxChecksumSupported ? 1 : 0;
    Info.TxTsoV4Supported = Adapter->TxTsoV4Supported ? 1 : 0;
    Info.TxTsoV6Supported = Adapter->TxTsoV6Supported ? 1 : 0;
    Info.TxChecksumV4Enabled = Adapter->TxChecksumV4Enabled ? 1 : 0;
    Info.TxChecksumV6Enabled = Adapter->TxChecksumV6Enabled ? 1 : 0;
    Info.TxTsoV4Enabled = Adapter->TxTsoV4Enabled ? 1 : 0;
    Info.TxTsoV6Enabled = Adapter->TxTsoV6Enabled ? 1 : 0;

    Info.StatTxPackets = Adapter->StatTxPackets;
    Info.StatTxBytes = Adapter->StatTxBytes;
    Info.StatRxPackets = Adapter->StatRxPackets;
    Info.StatRxBytes = Adapter->StatRxBytes;
    Info.StatTxErrors = Adapter->StatTxErrors;
    Info.StatRxErrors = Adapter->StatRxErrors;
    Info.StatRxNoBuffers = Adapter->StatRxNoBuffers;

    Info.RxVqErrorFlags = (ULONG)virtqueue_split_get_error_flags(&Adapter->RxVq.Vq);
    Info.TxVqErrorFlags = (ULONG)virtqueue_split_get_error_flags(&Adapter->TxVq.Vq);

    Info.TxTsoMaxOffloadSize = Adapter->TxTsoMaxOffloadSize;
    Info.TxUdpChecksumV4Enabled = Adapter->TxUdpChecksumV4Enabled ? 1 : 0;
    Info.TxUdpChecksumV6Enabled = Adapter->TxUdpChecksumV6Enabled ? 1 : 0;

    Info.CtrlVqNegotiated = (Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VQ) ? 1 : 0;
    Info.CtrlRxNegotiated = (Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_RX) ? 1 : 0;
    Info.CtrlVlanNegotiated = (Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_VLAN) ? 1 : 0;
    Info.CtrlMacAddrNegotiated = (Adapter->GuestFeatures & VIRTIO_NET_F_CTRL_MAC_ADDR) ? 1 : 0;

    Info.CtrlVqQueueIndex = Adapter->CtrlVq.QueueIndex;
    Info.CtrlVqQueueSize = Adapter->CtrlVq.QueueSize;
    Info.CtrlVqErrorFlags = (ULONG)virtqueue_split_get_error_flags(&Adapter->CtrlVq.Vq);

    Info.CtrlCmdSent = Adapter->StatCtrlVqCmdSent;
    Info.CtrlCmdOk = Adapter->StatCtrlVqCmdOk;
    Info.CtrlCmdErr = Adapter->StatCtrlVqCmdErr;
    Info.CtrlCmdTimeout = Adapter->StatCtrlVqCmdTimeout;
    Info.StatTxTcpCsumOffload = Adapter->StatTxTcpCsumOffload;
    Info.StatTxTcpCsumFallback = Adapter->StatTxTcpCsumFallback;
    Info.StatTxUdpCsumOffload = Adapter->StatTxUdpCsumOffload;
    Info.StatTxUdpCsumFallback = Adapter->StatTxUdpCsumFallback;

    RtlCopyMemory(Info.PermanentMac, Adapter->PermanentMac, ETH_LENGTH_OF_ADDRESS);
    RtlCopyMemory(Info.CurrentMac, Adapter->CurrentMac, ETH_LENGTH_OF_ADDRESS);
    Info.LinkUp = Adapter->LinkUp ? 1u : 0u;

    Info.InterruptCountVector0 = (ULONG)InterlockedCompareExchange(&Adapter->InterruptCountByVector[0], 0, 0);
    Info.InterruptCountVector1 = (ULONG)InterlockedCompareExchange(&Adapter->InterruptCountByVector[1], 0, 0);
    Info.InterruptCountVector2 = (ULONG)InterlockedCompareExchange(&Adapter->InterruptCountByVector[2], 0, 0);
    Info.DpcCountVector0 = (ULONG)InterlockedCompareExchange(&Adapter->DpcCountByVector[0], 0, 0);
    Info.DpcCountVector1 = (ULONG)InterlockedCompareExchange(&Adapter->DpcCountByVector[1], 0, 0);
    Info.DpcCountVector2 = (ULONG)InterlockedCompareExchange(&Adapter->DpcCountByVector[2], 0, 0);
    Info.RxBuffersDrained = (ULONG)InterlockedCompareExchange(&Adapter->RxBuffersDrained, 0, 0);
    Info.TxBuffersDrained = (ULONG)InterlockedCompareExchange(&Adapter->TxBuffersDrained, 0, 0);
    NdisReleaseSpinLock(&Adapter->Lock);

    // Read back the currently programmed MSI-X vectors from virtio common config.
    //
    // Only attempt this if:
    //  - we're at PASSIVE_LEVEL (IOCTL path)
    //  - BAR0 is still mapped (not surprise removed / not halted)
    if (KeGetCurrentIrql() == PASSIVE_LEVEL && CommonCfg != NULL && Adapter->State != AerovNetAdapterStopped && !Adapter->SurpriseRemoved) {
      KIRQL OldIrql;
      USHORT MsixConfig;
      USHORT MsixRx;
      USHORT MsixTx;

      MsixConfig = READ_REGISTER_USHORT((volatile USHORT*)&CommonCfg->msix_config);
      KeMemoryBarrier();

      MsixRx = VIRTIO_PCI_MSI_NO_VECTOR;
      MsixTx = VIRTIO_PCI_MSI_NO_VECTOR;

      KeAcquireSpinLock(&Adapter->Vdev.CommonCfgLock, &OldIrql);

      WRITE_REGISTER_USHORT((volatile USHORT*)&CommonCfg->queue_select, 0);
      KeMemoryBarrier();
      /*
       * Flush posted MMIO selector writes (see docs/windows7-virtio-driver-contract.md §1.5.0).
       * Without a readback, some platforms can observe the old queue_select value
       * when reading queue_msix_vector immediately after the write.
       */
      (VOID)READ_REGISTER_USHORT((volatile USHORT*)&CommonCfg->queue_select);
      KeMemoryBarrier();
      MsixRx = READ_REGISTER_USHORT((volatile USHORT*)&CommonCfg->queue_msix_vector);
      KeMemoryBarrier();

      WRITE_REGISTER_USHORT((volatile USHORT*)&CommonCfg->queue_select, 1);
      KeMemoryBarrier();
      (VOID)READ_REGISTER_USHORT((volatile USHORT*)&CommonCfg->queue_select);
      KeMemoryBarrier();
      MsixTx = READ_REGISTER_USHORT((volatile USHORT*)&CommonCfg->queue_msix_vector);
      KeMemoryBarrier();

      KeReleaseSpinLock(&Adapter->Vdev.CommonCfgLock, OldIrql);

      Info.MsixConfigVector = MsixConfig;
      Info.MsixRxVector = MsixRx;
      Info.MsixTxVector = MsixTx;

      // If vectors are assigned, treat the effective mode as MSI/MSI-X even if
      // `UseMsix` was not set (should be rare; included for observability).
      if (MsixConfig != VIRTIO_PCI_MSI_NO_VECTOR || MsixRx != VIRTIO_PCI_MSI_NO_VECTOR || MsixTx != VIRTIO_PCI_MSI_NO_VECTOR) {
        Info.InterruptMode = AEROVNET_INTERRUPT_MODE_MSI;
      }
    }

    CopyLen = OutLen;
    if (CopyLen > sizeof(Info)) {
      CopyLen = sizeof(Info);
    }

    RtlCopyMemory(Irp->AssociatedIrp.SystemBuffer, &Info, CopyLen);
    Status = STATUS_SUCCESS;
  } else {
    RtlZeroMemory(&OffloadStats, sizeof(OffloadStats));
    OffloadStats.Version = AEROVNET_OFFLOAD_STATS_VERSION;
    OffloadStats.Size = sizeof(OffloadStats);
    RtlCopyMemory(OffloadStats.Mac, Adapter->CurrentMac, ETH_LENGTH_OF_ADDRESS);
    OffloadStats.HostFeatures = Adapter->HostFeatures;
    OffloadStats.GuestFeatures = Adapter->GuestFeatures;
    OffloadStats.TxCsumOffloadTcp4 = Adapter->StatTxCsumOffloadTcp4;
    OffloadStats.TxCsumOffloadTcp6 = Adapter->StatTxCsumOffloadTcp6;
    OffloadStats.TxCsumOffloadUdp4 = Adapter->StatTxCsumOffloadUdp4;
    OffloadStats.TxCsumOffloadUdp6 = Adapter->StatTxCsumOffloadUdp6;
    OffloadStats.RxCsumValidatedTcp4 = Adapter->StatRxCsumValidatedTcp4;
    OffloadStats.RxCsumValidatedTcp6 = Adapter->StatRxCsumValidatedTcp6;
    OffloadStats.RxCsumValidatedUdp4 = Adapter->StatRxCsumValidatedUdp4;
    OffloadStats.RxCsumValidatedUdp6 = Adapter->StatRxCsumValidatedUdp6;
    OffloadStats.TxCsumFallback = Adapter->StatTxCsumFallback;

    NdisReleaseSpinLock(&Adapter->Lock);

    RtlCopyMemory(Irp->AssociatedIrp.SystemBuffer, &OffloadStats, sizeof(OffloadStats));
    CopyLen = sizeof(OffloadStats);
    Status = STATUS_SUCCESS;
  }

  AerovNetDiagDereferenceAdapter(Adapter);

  Irp->IoStatus.Status = Status;
  Irp->IoStatus.Information = CopyLen;
  IoCompleteRequest(Irp, IO_NO_INCREMENT);
  return Status;
}

static VOID AerovNetDriverUnload(_In_ PDRIVER_OBJECT DriverObject) {
  UNREFERENCED_PARAMETER(DriverObject);

  if (g_NdisDeviceHandle) {
    NdisDeregisterDeviceEx(g_NdisDeviceHandle);
    g_NdisDeviceHandle = NULL;
    g_NdisDeviceObject = NULL;
  }

  if (g_DiagLockInitialized) {
    NdisAcquireSpinLock(&g_DiagLock);
    g_DiagAdapter = NULL;
    NdisReleaseSpinLock(&g_DiagLock);

    NdisFreeSpinLock(&g_DiagLock);
    g_DiagLockInitialized = FALSE;
  }

  if (g_NdisDriverHandle) {
    NdisMDeregisterMiniportDriver(g_NdisDriverHandle);
    g_NdisDriverHandle = NULL;
  }
}

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath) {
  NDIS_STATUS Status;
  NDIS_MINIPORT_DRIVER_CHARACTERISTICS Ch;
  ULONG I;
  NDIS_DEVICE_OBJECT_ATTRIBUTES DevAttrs;
  UNICODE_STRING DeviceName;
  UNICODE_STRING SymbolicName;

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

  // Register a global diagnostics control device for user-mode state queries.
  //
  // This is best-effort: failure should not prevent the miniport from loading.
  NdisAllocateSpinLock(&g_DiagLock);
  g_DiagLockInitialized = TRUE;

  for (I = 0; I <= IRP_MJ_MAXIMUM_FUNCTION; I++) {
    g_DiagMajorFunctions[I] = AerovNetDiagDispatchDefault;
  }
  g_DiagMajorFunctions[IRP_MJ_CREATE] = AerovNetDiagDispatchCreateClose;
  g_DiagMajorFunctions[IRP_MJ_CLOSE] = AerovNetDiagDispatchCreateClose;
  g_DiagMajorFunctions[IRP_MJ_DEVICE_CONTROL] = AerovNetDiagDispatchDeviceControl;

  RtlInitUnicodeString(&DeviceName, AEROVNET_DIAG_DEVICE_NAME);
  RtlInitUnicodeString(&SymbolicName, AEROVNET_DIAG_SYMBOLIC_NAME);

  RtlZeroMemory(&DevAttrs, sizeof(DevAttrs));
  DevAttrs.Header.Type = NDIS_OBJECT_TYPE_DEVICE_OBJECT_ATTRIBUTES;
  DevAttrs.Header.Revision = NDIS_DEVICE_OBJECT_ATTRIBUTES_REVISION_1;
  DevAttrs.Header.Size = sizeof(DevAttrs);
  DevAttrs.MajorFunctions = g_DiagMajorFunctions;
  DevAttrs.ExtensionSize = 0;
  DevAttrs.DeviceName = &DeviceName;
  DevAttrs.SymbolicName = &SymbolicName;
  DevAttrs.DefaultSDDLString = (PWSTR)g_AerovNetDiagSddl;
  DevAttrs.DeviceClassGuid = NULL;

  Status = NdisRegisterDeviceEx(g_NdisDriverHandle, &DevAttrs, &g_NdisDeviceObject, &g_NdisDeviceHandle);
  if (Status != NDIS_STATUS_SUCCESS) {
#if DBG
    DbgPrint("aero_virtio_net: diag: NdisRegisterDeviceEx failed: 0x%08X\n", Status);
#endif
    g_NdisDeviceHandle = NULL;
    g_NdisDeviceObject = NULL;

    NdisFreeSpinLock(&g_DiagLock);
    g_DiagLockInitialized = FALSE;
  }
  if (g_NdisDeviceObject) {
    g_NdisDeviceObject->Flags |= DO_BUFFERED_IO;
    g_NdisDeviceObject->Flags &= ~DO_DEVICE_INITIALIZING;
  }

  return STATUS_SUCCESS;
}
