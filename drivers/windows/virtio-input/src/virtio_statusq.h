#pragma once

#include <ntddk.h>
#include <wdf.h>

#include "virtio_pci_modern.h"

typedef struct _VIRTIO_STATUSQ VIRTIO_STATUSQ, *PVIRTIO_STATUSQ;

NTSTATUS
VirtioStatusQInitialize(
    _Out_ PVIRTIO_STATUSQ* StatusQ,
    _In_ WDFDEVICE Device,
    _Inout_ PVIRTIO_PCI_DEVICE PciDevice,
    _In_ WDFDMAENABLER DmaEnabler,
    _In_ USHORT QueueIndex,
    _In_ USHORT QueueSize);

VOID
VirtioStatusQUninitialize(_In_ PVIRTIO_STATUSQ StatusQ);

VOID
VirtioStatusQReset(_In_ PVIRTIO_STATUSQ StatusQ);

VOID
VirtioStatusQGetRingAddresses(_In_ PVIRTIO_STATUSQ StatusQ, _Out_ UINT64* DescPa, _Out_ UINT64* AvailPa, _Out_ UINT64* UsedPa);

VOID
VirtioStatusQSetActive(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN Active);

VOID
VirtioStatusQSetDropOnFull(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN DropOnFull);

NTSTATUS
VirtioStatusQWriteKeyboardLedReport(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedBitfield);

VOID
VirtioStatusQProcessUsedBuffers(_In_ PVIRTIO_STATUSQ StatusQ);
