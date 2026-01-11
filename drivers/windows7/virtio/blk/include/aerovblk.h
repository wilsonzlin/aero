#pragma once

#include <ntddk.h>
#include <storport.h>
#include <scsi.h>
#include <ntddscsi.h>

#include "virtqueue_split.h"

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

#define AEROVBLK_QUEUE_INDEX 0u
#define AEROVBLK_QUEUE_SIZE 128u

/*
 * Clamp the SG count we advertise to StorPort and size the on-stack VIRTQ_SG
 * array accordingly. The device also advertises seg_max (data segments only).
 */
#define AEROVBLK_MAX_SG_ELEMENTS 128u

#define AEROVBLK_VIRTIO_PCI_REVISION_ID 0x01u

#define AEROVBLK_VIRTIO_PCI_BAR0_MIN_LEN 0x4000u

#define AEROVBLK_VIRTIO_PCI_COMMON_CFG_OFFSET 0x0000u
#define AEROVBLK_VIRTIO_PCI_NOTIFY_CFG_OFFSET 0x1000u
#define AEROVBLK_VIRTIO_PCI_ISR_CFG_OFFSET 0x2000u
#define AEROVBLK_VIRTIO_PCI_DEVICE_CFG_OFFSET 0x3000u

#define AEROVBLK_VIRTIO_PCI_NOTIFY_OFF_MULTIPLIER 4u

/* Virtio status bits (standard). */
#define VIRTIO_STATUS_ACKNOWLEDGE 0x01u
#define VIRTIO_STATUS_DRIVER 0x02u
#define VIRTIO_STATUS_DRIVER_OK 0x04u
#define VIRTIO_STATUS_FEATURES_OK 0x08u
#define VIRTIO_STATUS_FAILED 0x80u

/* Modern virtio feature bit indices. */
#define VIRTIO_F_VERSION_1 32u

#define VIRTIO_BLK_F_SEG_MAX 2u
#define VIRTIO_BLK_F_BLK_SIZE 6u
#define VIRTIO_BLK_F_FLUSH 9u

#define AEROVBLK_FEATURE_VERSION_1 (1ull << VIRTIO_F_VERSION_1)
#define AEROVBLK_FEATURE_RING_INDIRECT_DESC (1ull << VIRTIO_F_RING_INDIRECT_DESC)
#define AEROVBLK_FEATURE_BLK_SEG_MAX (1ull << VIRTIO_BLK_F_SEG_MAX)
#define AEROVBLK_FEATURE_BLK_BLK_SIZE (1ull << VIRTIO_BLK_F_BLK_SIZE)
#define AEROVBLK_FEATURE_BLK_FLUSH (1ull << VIRTIO_BLK_F_FLUSH)

#define VIRTIO_BLK_T_IN 0u
#define VIRTIO_BLK_T_OUT 1u
#define VIRTIO_BLK_T_FLUSH 4u

#define VIRTIO_BLK_S_OK 0u
#define VIRTIO_BLK_S_IOERR 1u
#define VIRTIO_BLK_S_UNSUPP 2u

#pragma pack(push, 1)
typedef struct _VIRTIO_BLK_REQ_HDR {
    ULONG Type;
    ULONG Ioprio;
    ULONGLONG Sector;
} VIRTIO_BLK_REQ_HDR, *PVIRTIO_BLK_REQ_HDR;

C_ASSERT(sizeof(VIRTIO_BLK_REQ_HDR) == 16);

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

    PSCSI_REQUEST_BLOCK Srb;
    BOOLEAN IsWrite;
} AEROVBLK_REQUEST_CONTEXT, *PAEROVBLK_REQUEST_CONTEXT;

typedef struct _AEROVBLK_DEVICE_EXTENSION {
    PUCHAR Bar0;
    ULONG Bar0Length;

    PUCHAR CommonCfg;
    PUCHAR NotifyBase;
    PUCHAR IsrStatus;
    PUCHAR DeviceCfg;

    ULONG NotifyOffMultiplier;
    USHORT QueueNotifyOff;

    VIRTQ_SPLIT* Vq;
    PVOID RingVa;
    PHYSICAL_ADDRESS RingPa;
    ULONG RingBytes;

    PVOID IndirectVa;
    PHYSICAL_ADDRESS IndirectPa;
    ULONG IndirectBytes;
    USHORT IndirectTableCount;
    USHORT IndirectMaxDesc;

    ULONGLONG NegotiatedFeatures;
    BOOLEAN SupportsIndirect;
    BOOLEAN SupportsFlush;

    ULONGLONG CapacitySectors;
    ULONG LogicalSectorSize;
    ULONG SegMax;

    PAEROVBLK_REQUEST_CONTEXT RequestContexts;
    ULONG RequestContextCount;
    LIST_ENTRY FreeRequestList;
    ULONG FreeRequestCount;

    BOOLEAN Removed;
    SENSE_DATA LastSense;
} AEROVBLK_DEVICE_EXTENSION, *PAEROVBLK_DEVICE_EXTENSION;

C_ASSERT(sizeof(VIRTQ_DESC) == 16);
C_ASSERT(AEROVBLK_QUEUE_SIZE == 128);

#define AEROVBLK_SRBIO_SIG "AEROVBLK"
#define AEROVBLK_IOCTL_QUERY 0x8000A001u

typedef struct _AEROVBLK_QUERY_INFO {
    ULONGLONG NegotiatedFeatures;
    USHORT QueueSize;
    USHORT NumFree;
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
