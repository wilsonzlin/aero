#pragma once

#include <ndis.h>

#include "../../common/include/virtio_bits.h"
#include "../../common/include/virtio_pci_legacy.h"
#include "../../common/include/virtio_queue.h"

// Driver identity
#define AEROVNET_VENDOR_ID 0x1AF4 // virtio vendor

#define AEROVNET_MTU_DEFAULT 1500u

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

typedef struct _VIRTIO_NET_CONFIG {
  UCHAR Mac[6];
  USHORT Status;
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
  struct _AEROVNET_ADAPTER* Adapter;

  PUCHAR HeaderVa;
  PHYSICAL_ADDRESS HeaderPa;

  PNET_BUFFER_LIST Nbl;
  PNET_BUFFER Nb;

  PSCATTER_GATHER_LIST SgList;
  USHORT DescHeadId;
} AEROVNET_TX_REQUEST;

typedef enum _AEROVNET_ADAPTER_STATE {
  AerovNetAdapterStopped = 0,
  AerovNetAdapterRunning,
  AerovNetAdapterPaused,
} AEROVNET_ADAPTER_STATE;

typedef struct _AEROVNET_ADAPTER {
  NDIS_HANDLE MiniportAdapterHandle;
  NDIS_HANDLE InterruptHandle;
  NDIS_HANDLE DmaHandle;
  NDIS_HANDLE NblPool;

  NDIS_SPIN_LOCK Lock;

  AEROVNET_ADAPTER_STATE State;
  BOOLEAN SurpriseRemoved;
  volatile LONG IsrStatus;
  volatile LONG OutstandingSgMappings;
  KEVENT OutstandingSgEvent;

  // PCI resources
  PUCHAR IoBase;
  ULONG IoLength;
  ULONG IoPortStart;

  // Virtio
  VIRTIO_PCI_DEVICE Vdev;
  VIRTIO_QUEUE RxVq;
  VIRTIO_QUEUE TxVq;

  ULONG HostFeatures;
  ULONG GuestFeatures;

  BOOLEAN LinkUp;

  UCHAR PermanentMac[ETH_LENGTH_OF_ADDRESS];
  UCHAR CurrentMac[ETH_LENGTH_OF_ADDRESS];

  ULONG PacketFilter;
  ULONG MulticastListSize;
  UCHAR MulticastList[NDIS_MAX_MULTICAST_LIST][ETH_LENGTH_OF_ADDRESS];

  ULONG Mtu;
  ULONG MaxFrameSize;
  ULONG RxBufferDataBytes;
  ULONG RxBufferTotalBytes;

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
} AEROVNET_ADAPTER;

// Helpers for per-NBL bookkeeping via MiniportReserved.
#define AEROVNET_NBL_SET_PENDING(_nbl, _val) ((_nbl)->MiniportReserved[0] = (PVOID)(ULONG_PTR)(_val))
#define AEROVNET_NBL_GET_PENDING(_nbl) ((LONG)(ULONG_PTR)((_nbl)->MiniportReserved[0]))

#define AEROVNET_NBL_SET_STATUS(_nbl, _val) ((_nbl)->MiniportReserved[1] = (PVOID)(ULONG_PTR)(_val))
#define AEROVNET_NBL_GET_STATUS(_nbl) ((NDIS_STATUS)(ULONG_PTR)((_nbl)->MiniportReserved[1]))
