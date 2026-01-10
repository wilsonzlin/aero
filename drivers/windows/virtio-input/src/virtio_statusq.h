#pragma once

#include <ntddk.h>
#include <wdf.h>

typedef struct _VIRTIO_STATUSQ VIRTIO_STATUSQ, *PVIRTIO_STATUSQ;

struct virtqueue;

NTSTATUS
VirtioStatusQInitialize(_Out_ PVIRTIO_STATUSQ* StatusQ, _In_ WDFDEVICE Device, _In_ struct virtqueue* Vq);

VOID
VirtioStatusQUninitialize(_In_ PVIRTIO_STATUSQ StatusQ);

VOID
VirtioStatusQSetActive(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN Active);

VOID
VirtioStatusQSetDropOnFull(_In_ PVIRTIO_STATUSQ StatusQ, _In_ BOOLEAN DropOnFull);

NTSTATUS
VirtioStatusQWriteKeyboardLedReport(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedBitfield);

VOID
VirtioStatusQProcessUsedBuffers(_In_ PVIRTIO_STATUSQ StatusQ);

