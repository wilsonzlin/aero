#pragma once

#include <ntddk.h>
#include <storport.h>
#include <scsi.h>
#include <ntddscsi.h>

#include "virtio_pci_modern_miniport.h"
#include "virtqueue_split_legacy.h"
#include "virtio_os_storport.h"

#include "aero_virtio_blk_ioctl.h"

#if DBG
#define AEROVBLK_LOG(fmt, ...) DbgPrint("aero_virtio_blk: " fmt "\n", __VA_ARGS__)
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

#define AEROVBLK_PCI_VENDOR_ID 0x1AF4u
#define AEROVBLK_PCI_DEVICE_ID 0x1042u
#define AEROVBLK_VIRTIO_PCI_REVISION_ID 0x01u

#define AEROVBLK_BAR0_MIN_LEN 0x4000u

#define VIRTIO_BLK_F_SEG_MAX 2u
#define VIRTIO_BLK_F_BLK_SIZE 6u
#define VIRTIO_BLK_F_FLUSH 9u

#define AEROVBLK_FEATURE_RING_INDIRECT_DESC (1ull << 28)
#define AEROVBLK_FEATURE_RING_EVENT_IDX (1ull << 29)
#define AEROVBLK_FEATURE_RING_PACKED (1ull << 34)
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

C_ASSERT(FIELD_OFFSET(VIRTIO_BLK_CONFIG, Capacity) == 0x00);
C_ASSERT(FIELD_OFFSET(VIRTIO_BLK_CONFIG, SizeMax) == 0x08);
C_ASSERT(FIELD_OFFSET(VIRTIO_BLK_CONFIG, SegMax) == 0x0C);
C_ASSERT(FIELD_OFFSET(VIRTIO_BLK_CONFIG, Cylinders) == 0x10);
C_ASSERT(FIELD_OFFSET(VIRTIO_BLK_CONFIG, Heads) == 0x12);
C_ASSERT(FIELD_OFFSET(VIRTIO_BLK_CONFIG, Sectors) == 0x13);
C_ASSERT(FIELD_OFFSET(VIRTIO_BLK_CONFIG, BlkSize) == 0x14);
C_ASSERT(sizeof(VIRTIO_BLK_CONFIG) == 0x18);

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
    VIRTIO_PCI_DEVICE Vdev;
    volatile UINT16* QueueNotifyAddrCache[1];

    /*
     * Interrupt mode selected by StorPort/PnP.
     *
     * When Windows assigns message-signaled interrupts (MSI/MSI-X), StorPort
     * invokes the miniport's HwMSInterruptRoutine and provides the message ID.
     * In that mode we must program virtio MSI-X vector routing (msix_config /
     * queue_msix_vector) and must not rely on the virtio ISR status byte.
     *
     * When message-signaled interrupts are not available, we fall back to INTx
     * (shared line) semantics and use the virtio ISR status byte as the
     * read-to-ack mechanism.
     */
    BOOLEAN UseMsi;
    USHORT MsiMessageCount;
    USHORT MsixConfigVector;
    USHORT MsixQueue0Vector;
    volatile UCHAR LastConfigGeneration;
    UCHAR Reserved1;

    virtio_os_ops_t VirtioOps;
    virtio_os_storport_ctx_t VirtioOpsCtx;

    virtqueue_split_t Vq;
    virtio_dma_buffer_t RingDma;

    ULONGLONG NegotiatedFeatures;
    BOOLEAN SupportsIndirect;
    BOOLEAN SupportsFlush;

    ULONGLONG CapacitySectors;
    ULONG LogicalSectorSize;
    ULONG SegMax;

    /*
     * Optional: count of capacity/config change events handled via the Virtio
     * CONFIG_INTERRUPT ISR bit (bit1). This is best-effort compatibility logic
     * for device models that violate the "static config" assumption.
     */
    ULONGLONG CapacityChangeEvents;

    PAEROVBLK_REQUEST_CONTEXT RequestContexts;
    ULONG RequestContextCount;
    LIST_ENTRY FreeRequestList;
    ULONG FreeRequestCount;

    /*
     * Set to 1 while the miniport is resetting/reinitializing the device/queue.
     * Used to reject new I/O submissions so StorPort can requeue them.
     */
    volatile LONG ResetInProgress;

    volatile LONG AbortSrbCount;
    volatile LONG ResetDeviceSrbCount;
    volatile LONG ResetBusSrbCount;
    volatile LONG PnpSrbCount;
    volatile LONG IoctlResetCount;

    volatile BOOLEAN Removed;
    /*
     * When set, the device may have disappeared (surprise removal / hot-unplug).
     * In that state, BAR0 MMIO access may fault, so hardware quiesce/reset must
     * be avoided.
     */
    volatile BOOLEAN SurpriseRemoved;
    SENSE_DATA LastSense;
} AEROVBLK_DEVICE_EXTENSION, *PAEROVBLK_DEVICE_EXTENSION;

C_ASSERT(AEROVBLK_QUEUE_SIZE == 128);

C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, NegotiatedFeatures) == 0x00);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, QueueSize) == 0x08);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, NumFree) == 0x0A);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, AvailIdx) == 0x0C);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, UsedIdx) == 0x0E);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, InterruptMode) == 0x10);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, MsixConfigVector) == 0x14);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, MsixQueue0Vector) == 0x16);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, MessageCount) == 0x18);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, Reserved0) == 0x1C);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, AbortSrbCount) == 0x20);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, ResetDeviceSrbCount) == 0x24);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, ResetBusSrbCount) == 0x28);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, PnpSrbCount) == 0x2C);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, IoctlResetCount) == 0x30);
C_ASSERT(FIELD_OFFSET(AEROVBLK_QUERY_INFO, CapacityChangeEvents) == 0x34);
C_ASSERT(sizeof(AEROVBLK_QUERY_INFO) == 0x38);

/* Minimum payload size for legacy callers (v1) that only expect the queue/feature fields. */
#define AEROVBLK_QUERY_INFO_V1_SIZE FIELD_OFFSET(AEROVBLK_QUERY_INFO, InterruptMode)

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
BOOLEAN AerovblkHwMSInterrupt(_In_ PVOID deviceExtension, _In_ ULONG messageId);
BOOLEAN AerovblkHwResetBus(_In_ PVOID deviceExtension, _In_ ULONG pathId);

SCSI_ADAPTER_CONTROL_STATUS AerovblkHwAdapterControl(
    _In_ PVOID deviceExtension,
    _In_ SCSI_ADAPTER_CONTROL_TYPE controlType,
    _In_ PVOID parameters);
