#pragma once

#include <ntddk.h>
#include <storport.h>

#define AEROVIRTIO_PCI_LEGACY_HOST_FEATURES 0x00
#define AEROVIRTIO_PCI_LEGACY_GUEST_FEATURES 0x04
#define AEROVIRTIO_PCI_LEGACY_QUEUE_PFN 0x08
#define AEROVIRTIO_PCI_LEGACY_QUEUE_NUM 0x0C
#define AEROVIRTIO_PCI_LEGACY_QUEUE_SEL 0x0E
#define AEROVIRTIO_PCI_LEGACY_QUEUE_NOTIFY 0x10
#define AEROVIRTIO_PCI_LEGACY_STATUS 0x12
#define AEROVIRTIO_PCI_LEGACY_ISR 0x13
#define AEROVIRTIO_PCI_LEGACY_CONFIG 0x14

#define AEROVIRTIO_STATUS_ACKNOWLEDGE 0x01
#define AEROVIRTIO_STATUS_DRIVER 0x02
#define AEROVIRTIO_STATUS_DRIVER_OK 0x04
#define AEROVIRTIO_STATUS_FEATURES_OK 0x08
#define AEROVIRTIO_STATUS_DEVICE_NEEDS_RESET 0x40
#define AEROVIRTIO_STATUS_FAILED 0x80

#define AEROVIRTIO_RING_F_INDIRECT_DESC (1u << 28)

#define AEROVIRTIO_BLK_F_BLK_SIZE (1u << 6)
#define AEROVIRTIO_BLK_F_FLUSH (1u << 9)

#define AEROVIRTIO_BLK_T_IN 0u
#define AEROVIRTIO_BLK_T_OUT 1u
#define AEROVIRTIO_BLK_T_FLUSH 4u

#define AEROVIRTIO_BLK_S_OK 0u
#define AEROVIRTIO_BLK_S_IOERR 1u
#define AEROVIRTIO_BLK_S_UNSUPP 2u

typedef enum _AEROVIRTIO_PCI_ACCESS_TYPE {
    AerovirtioPciAccessPort = 0,
    AerovirtioPciAccessMemory = 1,
} AEROVIRTIO_PCI_ACCESS_TYPE;

typedef struct _AEROVIRTIO_PCI_LEGACY_DEVICE {
    PUCHAR Base;
    ULONG Length;
    AEROVIRTIO_PCI_ACCESS_TYPE AccessType;
} AEROVIRTIO_PCI_LEGACY_DEVICE, *PAEROVIRTIO_PCI_LEGACY_DEVICE;

#pragma pack(push, 1)
typedef struct _AEROVIRTIO_BLK_REQ {
    ULONG type;
    ULONG reserved;
    ULONGLONG sector;
} AEROVIRTIO_BLK_REQ, *PAEROVIRTIO_BLK_REQ;

typedef struct _AEROVIRTIO_BLK_CONFIG {
    ULONGLONG capacity;
    ULONG size_max;
    ULONG seg_max;
    USHORT cylinders;
    UCHAR heads;
    UCHAR sectors;
    ULONG blk_size;
} AEROVIRTIO_BLK_CONFIG, *PAEROVIRTIO_BLK_CONFIG;
#pragma pack(pop)

UCHAR AerovirtioPciLegacyRead8(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset);
USHORT AerovirtioPciLegacyRead16(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset);
ULONG AerovirtioPciLegacyRead32(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset);

VOID AerovirtioPciLegacyWrite8(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset, _In_ UCHAR val);
VOID AerovirtioPciLegacyWrite16(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset, _In_ USHORT val);
VOID AerovirtioPciLegacyWrite32(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG offset, _In_ ULONG val);

VOID AerovirtioPciLegacyReset(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev);

UCHAR AerovirtioPciLegacyGetStatus(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev);
VOID AerovirtioPciLegacySetStatus(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ UCHAR status);

ULONG AerovirtioPciLegacyReadHostFeatures(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev);
VOID AerovirtioPciLegacyWriteGuestFeatures(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG features);

VOID AerovirtioPciLegacySelectQueue(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ USHORT queueIndex);
USHORT AerovirtioPciLegacyReadQueueSize(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev);
VOID AerovirtioPciLegacyWriteQueuePfn(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ ULONG queuePfn);
VOID AerovirtioPciLegacyNotifyQueue(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev, _In_ USHORT queueIndex);

UCHAR AerovirtioPciLegacyReadIsr(_In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev);

VOID AerovirtioPciLegacyReadDeviceConfig(
    _In_ PAEROVIRTIO_PCI_LEGACY_DEVICE dev,
    _In_ ULONG offset,
    _Out_writes_bytes_(len) PVOID buf,
    _In_ ULONG len);
