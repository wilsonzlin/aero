#pragma once

#include <ntddk.h>
#include <storport.h>
#include <scsi.h>
#include <ntddscsi.h>

#include "../../common/include/aerovirtio.h"

#if DBG
#define AEROVBLK_LOG(fmt, ...) DbgPrint("aerovblk: " fmt "\n", __VA_ARGS__)
#else
#define AEROVBLK_LOG(fmt, ...) \
    do {                       \
    } while (0)
#endif

#define AEROVBLK_PCI_VENDOR_ID 0x1AF4
#define AEROVBLK_PCI_DEVICE_ID 0x1001

#define AEROVBLK_LOGICAL_SECTOR_SIZE 512u

#define AEROVBLK_REQ_HDR_OFFSET 0u
#define AEROVBLK_REQ_STATUS_OFFSET 16u
#define AEROVBLK_REQ_INDIRECT_OFFSET 32u

#define AEROVBLK_MAX_INDIRECT_DESCS ((PAGE_SIZE - AEROVBLK_REQ_INDIRECT_OFFSET) / sizeof(AEROVIRTQ_DESC))

typedef struct _AEROVBLK_REQUEST_CONTEXT {
    PVOID SharedPageVa;
    STOR_PHYSICAL_ADDRESS SharedPagePa;

    volatile AEROVIRTIO_BLK_REQ* ReqHdr;
    volatile UCHAR* StatusByte;
    volatile AEROVIRTQ_DESC* IndirectDesc;

    PSCSI_REQUEST_BLOCK Srb;
    UCHAR ScsiOp;
    BOOLEAN IsWrite;
} AEROVBLK_REQUEST_CONTEXT, *PAEROVBLK_REQUEST_CONTEXT;

typedef struct _AEROVBLK_DEVICE_EXTENSION {
    AEROVIRTIO_PCI_LEGACY_DEVICE Pci;
    AEROVIRTQ Vq;

    ULONG NegotiatedFeatures;
    BOOLEAN SupportsIndirect;
    BOOLEAN SupportsFlush;

    ULONGLONG CapacitySectors;
    ULONG LogicalSectorSize;

    PAEROVBLK_REQUEST_CONTEXT RequestContexts;

    BOOLEAN Removed;
    SENSE_DATA LastSense;
} AEROVBLK_DEVICE_EXTENSION, *PAEROVBLK_DEVICE_EXTENSION;

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
