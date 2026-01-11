#pragma once

#include <ntddk.h>
#include <storport.h>
#include <scsi.h>
#include <ntddscsi.h>

/* Shared virtio headers (WDF-free). */
#include "aero_virtio_pci_modern.h" /* drivers/windows7/virtio-modern/common/include */
/* Explicit include to avoid picking up the legacy virtqueue header via include path order. */
#include "../../../windows/virtio/common/virtqueue_split.h"
#include "virtio_pci_cap_parser.h" /* drivers/win7/virtio/virtio-core/portable */
#include "virtio_pci_identity.h" /* drivers/win7/virtio/virtio-core/portable */

#if DBG
#define AEROVBLK_LOG(fmt, ...) DbgPrint("aerovblk: " fmt "\n", __VA_ARGS__)
#else
#define AEROVBLK_LOG(fmt, ...) \
    do {                       \
    } while (0)
#endif

/* -------------------------------------------------------------------------- */
/* Aero contract v1 constants                                                 */
/* -------------------------------------------------------------------------- */

#define AEROVBLK_PCI_REVISION_ID 0x01u

/* Contract v1: BAR0 is 64-bit MMIO, size 0x4000. */
#define AEROVBLK_BAR0_LENGTH_REQUIRED 0x4000u

/* Contract v1: single queue (requestq), index 0, size 128. */
#define AEROVBLK_QUEUE_INDEX 0u
#define AEROVBLK_QUEUE_SIZE 128u

/* Contract v1: notify_off_multiplier = 4 and queue_notify_off(q) = q. */
#define AEROVBLK_NOTIFY_OFF_MULTIPLIER_REQUIRED 4u

/* -------------------------------------------------------------------------- */
/* Virtio feature bits                                                        */
/* -------------------------------------------------------------------------- */

/* Ring feature bits (virtio spec, in low 32 bits). */
#define VIRTIO_F_RING_INDIRECT_DESC (1ui64 << 28)
#define VIRTIO_F_RING_EVENT_IDX (1ui64 << 29) /* must not be offered/negotiated in contract v1 */

/* virtio-blk feature bits (virtio spec, in low 32 bits). */
#define VIRTIO_BLK_F_SIZE_MAX (1ui64 << 1)
#define VIRTIO_BLK_F_SEG_MAX (1ui64 << 2)
#define VIRTIO_BLK_F_BLK_SIZE (1ui64 << 6)
#define VIRTIO_BLK_F_FLUSH (1ui64 << 9)

/* -------------------------------------------------------------------------- */
/* virtio-blk protocol                                                        */
/* -------------------------------------------------------------------------- */

#define AEROVBLK_LOGICAL_SECTOR_SIZE 512u /* virtio-blk always uses 512-byte sectors */

#define VIRTIO_BLK_T_IN 0u
#define VIRTIO_BLK_T_OUT 1u
#define VIRTIO_BLK_T_FLUSH 4u

#define VIRTIO_BLK_S_OK 0u
#define VIRTIO_BLK_S_IOERR 1u
#define VIRTIO_BLK_S_UNSUPP 2u

/* Request context shared-page layout. */
#define AEROVBLK_CTX_HDR_OFFSET 0u
#define AEROVBLK_CTX_STATUS_OFFSET 16u

/* Max data SG elements we allow Storport to hand us for a single SRB. */
#define AEROVBLK_MAX_DATA_SG 256u

#pragma pack(push, 1)
typedef struct _VIRTIO_BLK_REQ_HDR {
    ULONG Type;
    ULONG Reserved;
    ULONGLONG Sector;
} VIRTIO_BLK_REQ_HDR, *PVIRTIO_BLK_REQ_HDR;

typedef struct _VIRTIO_BLK_CONFIG {
    ULONGLONG Capacity; /* 512-byte sectors */
    ULONG SizeMax;
    ULONG SegMax;
    USHORT Cylinders;
    UCHAR Heads;
    UCHAR Sectors;
    ULONG BlkSize;
} VIRTIO_BLK_CONFIG, *PVIRTIO_BLK_CONFIG;
#pragma pack(pop)

typedef struct _AEROVBLK_REQUEST_CONTEXT {
    LIST_ENTRY Link;

    PVOID SharedPageVa;
    UINT64 SharedPagePa;

    volatile VIRTIO_BLK_REQ_HDR *ReqHdr;
    UINT64 ReqHdrPa;

    volatile UCHAR *StatusByte;
    UINT64 StatusPa;

    PSCSI_REQUEST_BLOCK Srb;
    BOOLEAN IsWrite;
} AEROVBLK_REQUEST_CONTEXT, *PAEROVBLK_REQUEST_CONTEXT;

typedef struct _AEROVBLK_DEVICE_EXTENSION {
    /* BAR0 MMIO mapping */
    PVOID Bar0Va;
    ULONG Bar0Length;

    /* Shared modern virtio-pci MMIO transport (contract v1). */
    AERO_VIRTIO_PCI_MODERN_DEVICE Vdev;

    /* Virtqueue (split ring) */
    VIRTQ_SPLIT *Vq;
    USHORT QueueSize;

    PVOID RingVa;
    UINT64 RingPa;
    ULONG RingSize;

    PVOID IndirectPoolVa;
    UINT64 IndirectPoolPa;
    ULONG IndirectPoolSize;
    USHORT IndirectMaxDesc;
    USHORT IndirectTableCount;

    /* Negotiated features (64-bit). */
    UINT64 NegotiatedFeatures;
    BOOLEAN SupportsFlush;

    /* Device properties */
    ULONGLONG CapacitySectors; /* 512-byte sectors */
    ULONG LogicalSectorSize; /* logical block size in bytes (blk_size feature) */
    ULONG SegMax; /* max data segments per request (seg_max feature) */
    ULONG SizeMax; /* max segment size (not used in contract v1; expected 0) */

    /* Per-request shared header/status buffers */
    PAEROVBLK_REQUEST_CONTEXT RequestContexts;
    ULONG RequestContextCount;
    LIST_ENTRY FreeRequestList;
    ULONG FreeRequestCount;

    BOOLEAN Removed;
    SENSE_DATA LastSense;
} AEROVBLK_DEVICE_EXTENSION, *PAEROVBLK_DEVICE_EXTENSION;

C_ASSERT(sizeof(VIRTIO_BLK_REQ_HDR) == 16);
C_ASSERT((AEROVBLK_CTX_STATUS_OFFSET % sizeof(ULONG)) == 0);

#define AEROVBLK_SRBIO_SIG "AEROVBLK"
#define AEROVBLK_IOCTL_QUERY 0x8000A001u

typedef struct _AEROVBLK_QUERY_INFO {
    UINT64 NegotiatedFeatures;
    USHORT QueueSize;
    USHORT NumFree;
    USHORT AvailIdx;
    USHORT UsedIdx;
    USHORT IndirectNumFree;
} AEROVBLK_QUERY_INFO, *PAEROVBLK_QUERY_INFO;

ULONG AerovblkHwFindAdapter(
    _In_ PVOID deviceExtension,
    _In_ PVOID hwContext,
    _In_ PVOID busInformation,
    _In_ PCHAR argumentString,
    _Inout_ PPORT_CONFIGURATION_INFORMATION configInfo,
    _Out_ PBOOLEAN again);

BOOLEAN AerovblkHwInitialize(_In_ PVOID deviceExtension);
BOOLEAN AerovblkHwStartIo(_In_ PVOID deviceExtension, _Inout_ PSCSI_REQUEST_BLOCK srb);
BOOLEAN AerovblkHwInterrupt(_In_ PVOID deviceExtension);
BOOLEAN AerovblkHwResetBus(_In_ PVOID deviceExtension, _In_ ULONG pathId);

SCSI_ADAPTER_CONTROL_STATUS AerovblkHwAdapterControl(
    _In_ PVOID deviceExtension,
    _In_ SCSI_ADAPTER_CONTROL_TYPE controlType,
    _In_ PVOID parameters);
