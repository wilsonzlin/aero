#pragma once

/*
 * Aero virtio-net (AERO-W7-VIRTIO v1) — Windows 7 SP1 NDIS 6.20 miniport
 *
 * Transport: virtio-pci modern (PCI caps + BAR0 MMIO), split virtqueues, INTx.
 *
 * Contract reference: docs/windows7-virtio-driver-contract.md (§3.2 virtio-net)
 */

#include <ndis.h>
#include <ntddk.h>

#include "../virtio/virtio-core/include/virtio_spec.h"
#include "../virtio/virtio-core/portable/virtio_pci_cap_parser.h"
#include "../virtio/virtio-core/portable/virtio_pci_identity.h"

#include "../../windows/virtio/common/virtqueue_split.h"

#define AEROVNET_TAG 'tNvA'

/* -------------------------------------------------------------------------- */
/* Contract / device IDs                                                      */
/* -------------------------------------------------------------------------- */

#define AEROVNET_PCI_VENDOR_ID 0x1AF4u
#define AEROVNET_PCI_DEVICE_ID 0x1041u
#define AEROVNET_PCI_REVISION_ID_V1 0x01u

/* Virtio feature bits (64-bit modern negotiation). */
#define VIRTIO_F_RING_INDIRECT_DESC (1ui64 << 28)
#define VIRTIO_F_RING_EVENT_IDX (1ui64 << 29)
#define VIRTIO_F_RING_PACKED (1ui64 << 34)

/* virtio-net feature bits (low 32). */
#define VIRTIO_NET_F_CSUM (1ui64 << 0)
#define VIRTIO_NET_F_GUEST_CSUM (1ui64 << 1)
#define VIRTIO_NET_F_MAC (1ui64 << 5)
#define VIRTIO_NET_F_GUEST_TSO4 (1ui64 << 7)
#define VIRTIO_NET_F_GUEST_TSO6 (1ui64 << 8)
#define VIRTIO_NET_F_GUEST_ECN (1ui64 << 9)
#define VIRTIO_NET_F_GUEST_UFO (1ui64 << 10)
#define VIRTIO_NET_F_HOST_TSO4 (1ui64 << 11)
#define VIRTIO_NET_F_HOST_TSO6 (1ui64 << 12)
#define VIRTIO_NET_F_HOST_ECN (1ui64 << 13)
#define VIRTIO_NET_F_HOST_UFO (1ui64 << 14)
#define VIRTIO_NET_F_MRG_RXBUF (1ui64 << 15)
#define VIRTIO_NET_F_STATUS (1ui64 << 16)
#define VIRTIO_NET_F_CTRL_VQ (1ui64 << 17)

/* virtio-net status bits (config.status) when VIRTIO_NET_F_STATUS negotiated. */
#define VIRTIO_NET_S_LINK_UP 1u

/* virtio-pci ISR status bits (read-to-ack). */
#define VIRTIO_PCI_ISR_QUEUE_INTERRUPT 0x01u
#define VIRTIO_PCI_ISR_CONFIG_INTERRUPT 0x02u

/* Contract v1 queue layout. */
#define AEROVNET_QUEUE_RX 0u
#define AEROVNET_QUEUE_TX 1u
#define AEROVNET_QUEUE_COUNT 2u
#define AEROVNET_QUEUE_SIZE 256u

/* Contract v1 frame size (no VLAN). */
#define AEROVNET_MTU 1500u
#define AEROVNET_MAX_FRAME_SIZE (AEROVNET_MTU + 14u) /* Ethernet header */
#define AEROVNET_MIN_FRAME_SIZE 14u

/* virtio-net header: always 10 bytes (no offloads). */
#define AEROVNET_NET_HDR_LEN 10u

/*
 * RX buffer contract:
 * - writable header (>=10 bytes)
 * - writable payload space (>=1514 bytes) following the header
 */
#define AEROVNET_RX_PAYLOAD_BYTES AEROVNET_MAX_FRAME_SIZE
#define AEROVNET_RX_BUFFER_BYTES (AEROVNET_NET_HDR_LEN + AEROVNET_RX_PAYLOAD_BYTES)

/* Maximum SG elements (payload only) we accept for TX DMA mapping. */
#define AEROVNET_MAX_TX_SG_ELEMENTS 32u

#pragma pack(push, 1)
typedef struct _VIRTIO_NET_HDR {
    UCHAR Flags;
    UCHAR GsoType;
    USHORT HdrLen;
    USHORT GsoSize;
    USHORT CsumStart;
    USHORT CsumOffset;
} VIRTIO_NET_HDR;
VIRTIO_STATIC_ASSERT(sizeof(VIRTIO_NET_HDR) == AEROVNET_NET_HDR_LEN, virtio_net_hdr_len);

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
    UINT16 DescHeadId;
} AEROVNET_TX_REQUEST;

typedef enum _AEROVNET_ADAPTER_STATE {
    AerovNetAdapterStopped = 0,
    AerovNetAdapterRunning,
    AerovNetAdapterPaused,
} AEROVNET_ADAPTER_STATE;

typedef struct _AEROVNET_VQ {
    USHORT QueueIndex;
    USHORT QueueSize;

    /* Split virtqueue state (allocated with VirtqSplitStateSize). */
    VIRTQ_SPLIT* Vq;

    /* Ring memory (DMA shared) backing desc/avail/used. */
    PVOID RingVa;
    PHYSICAL_ADDRESS RingPa;
    ULONG RingBytes;

    /* Indirect descriptor table pool (DMA shared). */
    PVOID IndirectVa;
    PHYSICAL_ADDRESS IndirectPa;
    ULONG IndirectBytes;

    /* Transport notify address for this queue (cached). */
    USHORT NotifyOff;
    volatile UINT16* NotifyAddr;
} AEROVNET_VQ;

typedef struct _AEROVNET_ADAPTER {
    NDIS_HANDLE MiniportAdapterHandle;
    NDIS_HANDLE InterruptHandle;
    NDIS_HANDLE DmaHandle;
    NDIS_HANDLE NblPool;

    NDIS_SPIN_LOCK Lock;

    AEROVNET_ADAPTER_STATE State;
    BOOLEAN SurpriseRemoved;

    /* ISR status accumulator (read-to-ack status byte copied from device). */
    volatile LONG PendingIsrStatus;

    volatile LONG OutstandingSgMappings;
    KEVENT OutstandingSgEvent;

    /* PCI config access (PCI_BUS_INTERFACE_STANDARD via IRP_MN_QUERY_INTERFACE). */
    PCI_BUS_INTERFACE_STANDARD PciInterface;
    BOOLEAN PciInterfaceAcquired;

    /* BAR0 MMIO mapping (virtio modern). */
    PUCHAR Bar0Va;
    PHYSICAL_ADDRESS Bar0Pa;
    ULONG Bar0Length;

    volatile virtio_pci_common_cfg* CommonCfg;
    volatile UCHAR* NotifyBase;
    ULONG NotifyOffMultiplier;
    volatile UCHAR* IsrStatus;
    volatile UCHAR* DeviceCfg;

    /* Virtio feature negotiation (64-bit). */
    UINT64 HostFeatures;
    UINT64 GuestFeatures;

    /* Queues */
    AEROVNET_VQ RxQ;
    AEROVNET_VQ TxQ;

    /* Link / MAC */
    BOOLEAN LinkUp;
    UCHAR PermanentMac[ETH_LENGTH_OF_ADDRESS];
    UCHAR CurrentMac[ETH_LENGTH_OF_ADDRESS];

    /* Packet filter */
    ULONG PacketFilter;
    ULONG MulticastListSize;
    UCHAR MulticastList[NDIS_MAX_MULTICAST_LIST][ETH_LENGTH_OF_ADDRESS];

    /* MTU / frame sizing */
    ULONG Mtu;
    ULONG MaxFrameSize;
    ULONG RxBufferDataBytes;
    ULONG RxBufferTotalBytes;

    /* Receive buffers */
    LIST_ENTRY RxFreeList;
    ULONG RxBufferCount;
    AEROVNET_RX_BUFFER* RxBuffers;

    /* Transmit requests */
    LIST_ENTRY TxFreeList;
    LIST_ENTRY TxAwaitingSgList;
    LIST_ENTRY TxPendingList;
    LIST_ENTRY TxSubmittedList;
    ULONG TxRequestCount;
    AEROVNET_TX_REQUEST* TxRequests;

    PUCHAR TxHeaderBlockVa;
    PHYSICAL_ADDRESS TxHeaderBlockPa;
    ULONG TxHeaderBlockBytes;

    /* Stats */
    ULONGLONG StatTxPackets;
    ULONGLONG StatTxBytes;
    ULONGLONG StatRxPackets;
    ULONGLONG StatRxBytes;
    ULONGLONG StatTxErrors;
    ULONGLONG StatRxErrors;
    ULONGLONG StatRxNoBuffers;
} AEROVNET_ADAPTER;

/* Helpers for per-NBL bookkeeping via MiniportReserved. */
#define AEROVNET_NBL_SET_PENDING(_nbl, _val) ((_nbl)->MiniportReserved[0] = (PVOID)(ULONG_PTR)(_val))
#define AEROVNET_NBL_GET_PENDING(_nbl) ((LONG)(ULONG_PTR)((_nbl)->MiniportReserved[0]))

#define AEROVNET_NBL_SET_STATUS(_nbl, _val) ((_nbl)->MiniportReserved[1] = (PVOID)(ULONG_PTR)(_val))
#define AEROVNET_NBL_GET_STATUS(_nbl) ((NDIS_STATUS)(ULONG_PTR)((_nbl)->MiniportReserved[1]))
