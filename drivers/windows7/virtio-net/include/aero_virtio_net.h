#pragma once

#include <ndis.h>

#include "virtio_pci_modern_miniport.h"
#include "virtqueue_split_legacy.h"
#include "virtio_os_ndis.h"

// Driver identity
#define AEROVNET_VENDOR_ID 0x1AF4 // virtio vendor
#define AEROVNET_PCI_DEVICE_ID 0x1041u

#define AEROVNET_MTU_DEFAULT 1500u

#define AEROVNET_PCI_REVISION_ID 0x01u

#define AEROVNET_BAR0_MIN_LEN 0x4000u

// Maximum number of MSI/MSI-X messages we track for per-vector diagnostics.
#ifndef AEROVNET_MSIX_MAX_MESSAGES
#define AEROVNET_MSIX_MAX_MESSAGES 8u
#endif

// Virtio feature bits (as masks).
#define AEROVNET_FEATURE_RING_INDIRECT_DESC ((UINT64)VIRTIO_RING_F_INDIRECT_DESC)
#define AEROVNET_FEATURE_RING_EVENT_IDX ((UINT64)VIRTIO_RING_F_EVENT_IDX)
#define AEROVNET_FEATURE_RING_PACKED ((UINT64)1ull << 34)

// Virtio-net feature bits (lower 32 bits).
#define VIRTIO_NET_F_CSUM      (1u << 0)
#define VIRTIO_NET_F_GUEST_CSUM (1u << 1)
#define VIRTIO_NET_F_MAC       (1u << 5)
#define VIRTIO_NET_F_GSO       (1u << 6)
#define VIRTIO_NET_F_GUEST_TSO4 (1u << 7)
#define VIRTIO_NET_F_GUEST_TSO6 (1u << 8)
#define VIRTIO_NET_F_GUEST_ECN  (1u << 9)
#define VIRTIO_NET_F_GUEST_UFO  (1u << 10)
#define VIRTIO_NET_F_HOST_TSO4  (1u << 11)
#define VIRTIO_NET_F_HOST_TSO6  (1u << 12)
#define VIRTIO_NET_F_HOST_ECN   (1u << 13)
#define VIRTIO_NET_F_HOST_UFO   (1u << 14)
#define VIRTIO_NET_F_MRG_RXBUF  (1u << 15)
#define VIRTIO_NET_F_STATUS     (1u << 16)
#define VIRTIO_NET_F_CTRL_VQ    (1u << 17)
#define VIRTIO_NET_F_CTRL_RX    (1u << 18)
#define VIRTIO_NET_F_CTRL_VLAN  (1u << 19)
#define VIRTIO_NET_F_CTRL_RX_EXTRA (1u << 20)
#define VIRTIO_NET_F_GUEST_ANNOUNCE (1u << 21)
#define VIRTIO_NET_F_MQ         (1u << 22)
#define VIRTIO_NET_F_CTRL_MAC_ADDR (1u << 23)

// virtio-net control virtqueue (VIRTIO_NET_F_CTRL_VQ) protocol.
#define VIRTIO_NET_OK 0u
#define VIRTIO_NET_ERR 1u

#define VIRTIO_NET_CTRL_RX 0u
#define VIRTIO_NET_CTRL_MAC 1u
#define VIRTIO_NET_CTRL_VLAN 2u
#define VIRTIO_NET_CTRL_ANNOUNCE 3u
#define VIRTIO_NET_CTRL_MQ 4u

#define VIRTIO_NET_CTRL_RX_PROMISC 0u
#define VIRTIO_NET_CTRL_RX_ALLMULTI 1u
#define VIRTIO_NET_CTRL_RX_ALLUNI 2u
#define VIRTIO_NET_CTRL_RX_NOMULTI 3u
#define VIRTIO_NET_CTRL_RX_NOUNI 4u
#define VIRTIO_NET_CTRL_RX_NOBCAST 5u

#define VIRTIO_NET_CTRL_MAC_TABLE_SET 0u
#define VIRTIO_NET_CTRL_MAC_ADDR_SET 1u

#define VIRTIO_NET_CTRL_VLAN_ADD 0u
#define VIRTIO_NET_CTRL_VLAN_DEL 1u

#pragma pack(push, 1)
typedef struct _VIRTIO_NET_CTRL_HDR {
  UCHAR Class;
  UCHAR Command;
} VIRTIO_NET_CTRL_HDR;

C_ASSERT(sizeof(VIRTIO_NET_CTRL_HDR) == 2);
#pragma pack(pop)

// virtio-net device status bits (config.status) if VIRTIO_NET_F_STATUS is negotiated.
#define VIRTIO_NET_S_LINK_UP 1u

#pragma pack(push, 1)
typedef struct _VIRTIO_NET_HDR {
  UCHAR Flags;
  UCHAR GsoType;
  USHORT HdrLen;
  USHORT GsoSize;
  USHORT CsumStart;
  USHORT CsumOffset;
} VIRTIO_NET_HDR;

C_ASSERT(sizeof(VIRTIO_NET_HDR) == 10);

// Receive header when VIRTIO_NET_F_MRG_RXBUF is negotiated.
// The driver must read NumBuffers from the first buffer of each received packet.
typedef struct _VIRTIO_NET_HDR_MRG_RXBUF {
  VIRTIO_NET_HDR Hdr;
  USHORT NumBuffers;
} VIRTIO_NET_HDR_MRG_RXBUF;

C_ASSERT(sizeof(VIRTIO_NET_HDR_MRG_RXBUF) == 12);
C_ASSERT(FIELD_OFFSET(VIRTIO_NET_HDR_MRG_RXBUF, NumBuffers) == sizeof(VIRTIO_NET_HDR));
// virtio-net per-packet header flags (virtio spec `virtio_net_hdr.flags`).
// These are used on both TX and RX when checksum/GSO features are negotiated.
#define VIRTIO_NET_HDR_F_NEEDS_CSUM 0x01u
#define VIRTIO_NET_HDR_F_DATA_VALID 0x02u

typedef struct _VIRTIO_NET_CONFIG {
  UCHAR Mac[6];
  USHORT Status;
  USHORT MaxVirtqueuePairs;
} VIRTIO_NET_CONFIG;
#pragma pack(pop)

typedef struct _AEROVNET_RX_BUFFER {
  LIST_ENTRY Link;

  PUCHAR BufferVa;
  PHYSICAL_ADDRESS BufferPa;
  ULONG BufferBytes;

  PMDL Mdl;
  PNET_BUFFER_LIST Nbl;
  PNET_BUFFER Nb;

  // When mergeable RX buffers are used, a single received frame may span
  // multiple posted buffers. The buffers are linked via PacketNext and are
  // returned to the free list together when the indicated NBL is returned.
  struct _AEROVNET_RX_BUFFER* PacketNext;
  ULONG PacketBytes;

  BOOLEAN Indicated;
} AEROVNET_RX_BUFFER;

typedef enum _AEROVNET_TX_STATE {
  AerovNetTxFree = 0,
  AerovNetTxAwaitingSg,
  AerovNetTxPendingSubmit,
  AerovNetTxSubmitted,
} AEROVNET_TX_STATE;

struct _AEROVNET_ADAPTER;

typedef struct _AEROVNET_TX_REQUEST {
  LIST_ENTRY Link;

  AEROVNET_TX_STATE State;
  BOOLEAN Cancelled;
  BOOLEAN HeaderBuilt;
  struct _AEROVNET_ADAPTER* Adapter;

  // Snapshot of NDIS-requested TX offload enablement at the time this request
  // was accepted. These flags can change at runtime via OID_TCP_OFFLOAD_PARAMETERS,
  // so queued/pending sends must not consult the live adapter config.
  BOOLEAN TxChecksumV4Enabled;
  BOOLEAN TxChecksumV6Enabled;
  BOOLEAN TxUdpChecksumV4Enabled;
  BOOLEAN TxUdpChecksumV6Enabled;
  BOOLEAN TxTsoV4Enabled;
  BOOLEAN TxTsoV6Enabled;

  PUCHAR HeaderVa;
  PHYSICAL_ADDRESS HeaderPa;

  PNET_BUFFER_LIST Nbl;
  PNET_BUFFER Nb;

  PSCATTER_GATHER_LIST SgList;
} AEROVNET_TX_REQUEST;

typedef enum _AEROVNET_ADAPTER_STATE {
  AerovNetAdapterStopped = 0,
  AerovNetAdapterRunning,
  AerovNetAdapterPaused,
} AEROVNET_ADAPTER_STATE;

typedef struct _AEROVNET_VQ {
  USHORT QueueIndex;
  USHORT QueueSize;

  virtio_dma_buffer_t RingDma;
  virtqueue_split_t Vq;
} AEROVNET_VQ;

typedef struct _AEROVNET_ADAPTER {
  NDIS_HANDLE MiniportAdapterHandle;
  NDIS_HANDLE InterruptHandle;
  NDIS_HANDLE DmaHandle;
  NDIS_HANDLE NblPool;

  // Interrupt mode selected from translated resources (INTx fallback retained).
  BOOLEAN UseMsix;
  BOOLEAN MsixAllOnVector0;
  USHORT MsixMessageCount;
  USHORT MsixConfigVector;
  USHORT MsixRxVector;
  USHORT MsixTxVector;
  BOOLEAN MsixVectorProgrammingFailed;

  NDIS_SPIN_LOCK Lock;
  // Serialize synchronous ctrl_vq commands. AerovNetCtrlSendCommand polls for
  // completion and frees requests; allowing concurrent callers can lead to one
  // caller freeing another caller's request. Keep a single in-flight command to
  // avoid spurious timeouts and use-after-free.
  KEVENT CtrlCmdEvent;

  AEROVNET_ADAPTER_STATE State;
  volatile BOOLEAN SurpriseRemoved;
  volatile LONG IsrStatus;

  volatile LONG OutstandingSgMappings;
  KEVENT OutstandingSgEvent;
  volatile LONG DiagRefCount;
  KEVENT DiagRefEvent;

  volatile LONG InterruptCountByVector[AEROVNET_MSIX_MAX_MESSAGES];
  volatile LONG DpcCountByVector[AEROVNET_MSIX_MAX_MESSAGES];
  volatile LONG RxBuffersDrained;
  volatile LONG TxBuffersDrained;

  UCHAR PciCfgSpace[256];

  // PCI BAR0 MMIO resources
  PHYSICAL_ADDRESS Bar0Pa;
  PUCHAR Bar0Va;
  ULONG Bar0Length;

  // Virtio-pci modern transport (vendor caps + BAR0 MMIO).
  VIRTIO_PCI_DEVICE Vdev;
  volatile UINT16* QueueNotifyAddrCache[8];

  // Virtqueues
  AEROVNET_VQ RxVq;
  AEROVNET_VQ TxVq;
  AEROVNET_VQ CtrlVq;
  LIST_ENTRY CtrlPendingList;

  // virtqueue_split OS shim
  virtio_os_ops_t VirtioOps;
  virtio_os_ndis_ctx_t VirtioOpsCtx;

  // Optional per-device registry key for exposing ctrl_vq diagnostics to the
  // guest selftest (best-effort).
  HANDLE CtrlVqRegKey;

  UINT64 HostFeatures;
  UINT64 GuestFeatures;

  // Negotiated virtio offload feature flags and current enablement state.
  BOOLEAN TxChecksumSupported;
  BOOLEAN TxTsoV4Supported;
  BOOLEAN TxTsoV6Supported;

  BOOLEAN TxChecksumV4Enabled;
  BOOLEAN TxChecksumV6Enabled;
  BOOLEAN TxUdpChecksumV4Enabled;
  BOOLEAN TxUdpChecksumV6Enabled;
  BOOLEAN TxTsoV4Enabled;
  BOOLEAN TxTsoV6Enabled;

  // Runtime RX checksum indication enable flags (controlled by OID_TCP_OFFLOAD_PARAMETERS).
  // These control whether the miniport sets TcpIpChecksumNetBufferListInfo for received
  // frames where the device reported checksum validation.
  BOOLEAN RxChecksumV4Enabled;
  BOOLEAN RxChecksumV6Enabled;
  BOOLEAN RxUdpChecksumV4Enabled;
  BOOLEAN RxUdpChecksumV6Enabled;

  ULONG TxTsoMaxOffloadSize;

  BOOLEAN LinkUp;

  UCHAR PermanentMac[ETH_LENGTH_OF_ADDRESS];
  UCHAR CurrentMac[ETH_LENGTH_OF_ADDRESS];

  ULONG PacketFilter;
  ULONG MulticastListSize;
  UCHAR MulticastList[NDIS_MAX_MULTICAST_LIST][ETH_LENGTH_OF_ADDRESS];

  ULONG Mtu;
  ULONG MaxFrameSize;
  // virtio-net header length in bytes (10-byte virtio_net_hdr or 12-byte
  // virtio_net_hdr_mrg_rxbuf). When VIRTIO_NET_F_MRG_RXBUF is negotiated, this
  // applies to both RX and TX descriptor chains.
  ULONG RxHeaderBytes;
  ULONG RxBufferDataBytes;
  ULONG RxBufferTotalBytes;
  // Scratch buffer used for reassembling multi-buffer RX frames into a single
  // contiguous byte range for checksum header parsing (NdisGetDataBuffer fallback).
  // Allocated from nonpaged pool so it is usable at DISPATCH_LEVEL.
  // Best-effort: if allocation fails, checksum indication for multi-buffer
  // receives is skipped. Only allocated when mergeable RX buffers and guest
  // checksum reporting are negotiated (VIRTIO_NET_F_MRG_RXBUF + VIRTIO_NET_F_GUEST_CSUM).
  PUCHAR RxChecksumScratch;
  ULONG RxChecksumScratchBytes;

  // Receive buffers
  LIST_ENTRY RxFreeList;
  ULONG RxBufferCount;
  AEROVNET_RX_BUFFER* RxBuffers;

  // Transmit requests
  LIST_ENTRY TxFreeList;
  LIST_ENTRY TxAwaitingSgList;
  LIST_ENTRY TxPendingList;
  LIST_ENTRY TxSubmittedList;
  ULONG TxRequestCount;
  AEROVNET_TX_REQUEST* TxRequests;
  PUCHAR TxHeaderBlockVa;
  PHYSICAL_ADDRESS TxHeaderBlockPa;
  ULONG TxHeaderBlockBytes;

  // Stats
  ULONGLONG StatTxPackets;
  ULONGLONG StatTxBytes;
  ULONGLONG StatRxPackets;
  ULONGLONG StatRxBytes;
  ULONGLONG StatTxErrors;
  ULONGLONG StatRxErrors;
  ULONGLONG StatRxNoBuffers;
  ULONGLONG StatTxTcpCsumOffload;
  ULONGLONG StatTxTcpCsumFallback;
  ULONGLONG StatTxUdpCsumOffload;
  ULONGLONG StatTxUdpCsumFallback;
  ULONGLONG StatCtrlVqCmdSent;
  ULONGLONG StatCtrlVqCmdOk;
  ULONGLONG StatCtrlVqCmdErr;
  ULONGLONG StatCtrlVqCmdTimeout;

  // Checksum offload counters (per-adapter).
  // - tx: packets where the driver asked the device to compute L4 checksum (virtio_net_hdr NEEDS_CSUM)
  // - rx: packets where the device reported checksum validation (virtio_net_hdr DATA_VALID)
  // - fallback: packets where checksum offload was requested by the OS but the driver computed it in software
  ULONGLONG StatTxCsumOffloadTcp4;
  ULONGLONG StatTxCsumOffloadTcp6;
  ULONGLONG StatTxCsumOffloadUdp4;
  ULONGLONG StatTxCsumOffloadUdp6;
  ULONGLONG StatRxCsumValidatedTcp4;
  ULONGLONG StatRxCsumValidatedTcp6;
  ULONGLONG StatRxCsumValidatedUdp4;
  ULONGLONG StatRxCsumValidatedUdp6;
  ULONGLONG StatTxCsumFallback;

  // Global adapter list link (for control IOCTL queries).
  LIST_ENTRY GlobalLink;
  BOOLEAN InGlobalList;
} AEROVNET_ADAPTER;

// Helpers for per-NBL bookkeeping via MiniportReserved.
#define AEROVNET_NBL_SET_PENDING(_nbl, _val) ((_nbl)->MiniportReserved[0] = (PVOID)(ULONG_PTR)(_val))
#define AEROVNET_NBL_GET_PENDING(_nbl) ((LONG)(ULONG_PTR)((_nbl)->MiniportReserved[0]))

#define AEROVNET_NBL_SET_STATUS(_nbl, _val) ((_nbl)->MiniportReserved[1] = (PVOID)(ULONG_PTR)(_val))
#define AEROVNET_NBL_GET_STATUS(_nbl) ((NDIS_STATUS)(ULONG_PTR)((_nbl)->MiniportReserved[1]))

// User-mode diagnostics IOCTL surface.
//
// The virtio-net miniport registers a global read-only diagnostics device (currently
// `\\.\AeroVirtioNetDiag`). This IOCTL exists so the guest selftest and host harness can
// observe checksum offload behavior in a black-box manner.

#define AEROVNET_OFFLOAD_STATS_VERSION 1u

typedef struct _AEROVNET_OFFLOAD_STATS {
  ULONG Version;
  ULONG Size;

  // Adapter identity.
  UCHAR Mac[ETH_LENGTH_OF_ADDRESS];
  UCHAR _Reserved0[2];

  // Negotiated virtio feature sets (raw bitmasks).
  ULONGLONG HostFeatures;
  ULONGLONG GuestFeatures;

  // Counters.
  ULONGLONG TxCsumOffloadTcp4;
  ULONGLONG TxCsumOffloadTcp6;
  ULONGLONG TxCsumOffloadUdp4;
  ULONGLONG TxCsumOffloadUdp6;
  ULONGLONG RxCsumValidatedTcp4;
  ULONGLONG RxCsumValidatedTcp6;
  ULONGLONG RxCsumValidatedUdp4;
  ULONGLONG RxCsumValidatedUdp6;
  ULONGLONG TxCsumFallback;
} AEROVNET_OFFLOAD_STATS;

// IOCTL: query checksum offload stats for the first adapter bound to this driver.
// - Input: none
// - Output: AEROVNET_OFFLOAD_STATS
#define AEROVNET_IOCTL_QUERY_OFFLOAD_STATS CTL_CODE(FILE_DEVICE_NETWORK, 0xA80, METHOD_BUFFERED, FILE_READ_ACCESS)
