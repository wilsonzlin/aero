#pragma once

#include <ntddk.h>
#include <storport.h>
#include <scsi.h>
#include <ntddscsi.h>

#include "../../common/include/virtio_bits.h"
#include "../../common/include/virtio_pci_legacy.h"
#include "../../common/include/virtio_queue.h"

#if DBG
#define AEROVBLK_LOG(fmt, ...) DbgPrint("aerovblk: " fmt "\n", __VA_ARGS__)
#else
#define AEROVBLK_LOG(fmt, ...) \
    do {                       \
    } while (0)
#endif

#define AEROVBLK_LOGICAL_SECTOR_SIZE 512u

#define AEROVBLK_CTX_HDR_OFFSET 0u
#define AEROVBLK_CTX_STATUS_OFFSET 16u
#define AEROVBLK_CTX_TABLE_OFFSET 32u

#define AEROVBLK_MAX_TABLE_DESCS ((PAGE_SIZE - AEROVBLK_CTX_TABLE_OFFSET) / sizeof(VRING_DESC))
#define AEROVBLK_MAX_SG_ELEMENTS (AEROVBLK_MAX_TABLE_DESCS - 2u)

#define VIRTIO_BLK_F_BLK_SIZE (1u << 6)
#define VIRTIO_BLK_F_FLUSH (1u << 9)
#define VIRTIO_BLK_F_SIZE_MAX (1u << 1)
#define VIRTIO_BLK_F_SEG_MAX (1u << 2)

#define VIRTIO_BLK_T_IN 0u
#define VIRTIO_BLK_T_OUT 1u
#define VIRTIO_BLK_T_FLUSH 4u

#define VIRTIO_BLK_S_OK 0u
#define VIRTIO_BLK_S_IOERR 1u
#define VIRTIO_BLK_S_UNSUPP 2u

#pragma pack(push, 1)
typedef struct _VIRTIO_BLK_REQ_HDR {
    ULONG Type;
    ULONG Reserved;
    ULONGLONG Sector;
} VIRTIO_BLK_REQ_HDR, *PVIRTIO_BLK_REQ_HDR;

typedef struct _VIRTIO_BLK_CONFIG {
    ULONGLONG Capacity;
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
    PHYSICAL_ADDRESS SharedPagePa;

    volatile VIRTIO_BLK_REQ_HDR* ReqHdr;
    volatile UCHAR* StatusByte;
    volatile VRING_DESC* TableDesc;
    PHYSICAL_ADDRESS TableDescPa;
    volatile VIRTIO_SG_ENTRY* Sg;

    PSCSI_REQUEST_BLOCK Srb;
    BOOLEAN IsWrite;
} AEROVBLK_REQUEST_CONTEXT, *PAEROVBLK_REQUEST_CONTEXT;

typedef struct _AEROVBLK_DEVICE_EXTENSION {
    VIRTIO_PCI_DEVICE Vdev;
    VIRTIO_QUEUE Vq;

    ULONG NegotiatedFeatures;
    BOOLEAN SupportsIndirect;
    BOOLEAN SupportsFlush;

    ULONGLONG CapacitySectors;
    ULONG LogicalSectorSize;
    ULONG SegMax;
    ULONG SizeMax;

    PAEROVBLK_REQUEST_CONTEXT RequestContexts;
    ULONG RequestContextCount;
    LIST_ENTRY FreeRequestList;
    ULONG FreeRequestCount;

    BOOLEAN Removed;
    SENSE_DATA LastSense;
} AEROVBLK_DEVICE_EXTENSION, *PAEROVBLK_DEVICE_EXTENSION;

C_ASSERT(sizeof(VRING_DESC) == 16);
C_ASSERT(sizeof(VIRTIO_SG_ENTRY) <= sizeof(VRING_DESC));
C_ASSERT((AEROVBLK_CTX_TABLE_OFFSET % sizeof(ULONGLONG)) == 0);

#define AEROVBLK_SRBIO_SIG "AEROVBLK"
#define AEROVBLK_IOCTL_QUERY 0x8000A001u

typedef struct _AEROVBLK_QUERY_INFO {
    ULONG NegotiatedFeatures;
    USHORT QueueSize;
    USHORT FreeCount;
    USHORT AvailIdx;
    USHORT UsedIdx;
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
