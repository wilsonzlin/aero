#pragma once

#include "hid_translate.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Portable helpers (host-buildable unit tests).
 *
 * The KMDF driver builds virtio-input status queue buffers for guest->device
 * output events (currently keyboard LEDs). Some helper logic is intentionally
 * kept in portable C so it can be unit-tested on the host without the WDK:
 *   - used-buffer cookie validation (cookie -> Tx buffer index)
 *   - coalescing behavior for pending LED writes
 */

enum { VIOINPUT_STATUSQ_EVENTS_PER_BUFFER = 6 };
enum { VIOINPUT_STATUSQ_LED_MASK_ALL = 0x1Fu }; /* bits 0..4 */

/*
 * Validates that a used-buffer cookie corresponds to a Tx buffer start address,
 * and returns the buffer index.
 *
 * This uses integer address arithmetic (uintptr_t) to remain robust even if
 * the cookie is corrupted (avoids undefined behavior from pointer comparison
 * across unrelated objects).
 */
bool VirtioStatusQCookieToIndex(
    const void* TxBase,
    size_t TxStride,
    uint16_t TxBufferCount,
    const void* Cookie,
    uint16_t* IndexOut);

/*
 * Coalescing model used by unit tests.
 *
 * This intentionally mirrors the driver's "PendingLedBitfield" behavior:
 * - A write submits immediately if there is a free buffer slot.
 * - If the queue is full, the latest LED bitfield is retained in Pending* and
 *   submitted on the next completion (or dropped if DropOnFull is enabled).
 */
typedef struct _VIOINPUT_STATUSQ_COALESCE_SIM {
    uint16_t Capacity;
    uint16_t FreeCount;
    bool DropOnFull;
    bool PendingValid;
    uint8_t PendingLedBitfield;
} VIOINPUT_STATUSQ_COALESCE_SIM;

void VirtioStatusQCoalesceSimInit(VIOINPUT_STATUSQ_COALESCE_SIM* Sim, uint16_t Capacity, bool DropOnFull);
bool VirtioStatusQCoalesceSimWrite(VIOINPUT_STATUSQ_COALESCE_SIM* Sim, uint8_t LedBitfield);
bool VirtioStatusQCoalesceSimComplete(VIOINPUT_STATUSQ_COALESCE_SIM* Sim);

#ifdef _WIN32

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

/*
 * Sets the supported EV_LED code mask for keyboard LED output reports.
 *
 * Mask is interpreted as: bit N => LED code N supported (codes 0..4).
 */
VOID
VirtioStatusQSetKeyboardLedSupportedMask(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedSupportedMask);

NTSTATUS
VirtioStatusQWriteKeyboardLedReport(_In_ PVIRTIO_STATUSQ StatusQ, _In_ UCHAR LedBitfield);

VOID
VirtioStatusQProcessUsedBuffers(_In_ PVIRTIO_STATUSQ StatusQ);

#endif /* _WIN32 */

#ifdef __cplusplus
} /* extern "C" */
#endif
