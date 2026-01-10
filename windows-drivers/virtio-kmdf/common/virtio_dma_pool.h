#pragma once

/*
 * DMA-safe pool of small, per-request buffers for virtio KMDF drivers.
 *
 * Motivation:
 *   - Virtio request headers/status and indirect descriptor tables must live in
 *     DMAable memory with a stable device address.
 *   - Indirect descriptor tables must also be physically contiguous because the
 *     device reads them sequentially.
 *   - Calling WdfCommonBufferCreate* for every request is too expensive; this
 *     module amortizes that cost by preallocating a fixed number of slots.
 *
 * Design:
 *   Option A) One big WDFCOMMONBUFFER is allocated up-front and split into
 *   fixed-size, fixed-alignment slots.
 *
 *   This guarantees physical contiguity for each slot and allows allocation/
 *   free at DISPATCH_LEVEL using a spinlock + allocation bitmap.
 */

#include "virtio_dma.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct _VIRTIO_DMA_POOL VIRTIO_DMA_POOL;

/*
 * The pool needs access to a WDFDMAENABLER. By default, we use the DMA enabler
 * stored in the VIRTIO_DMA_CONTEXT created by VirtioDmaCreate() (virtio_dma.h).
 *
 * Drivers with a different DMA context layout can override this macro (e.g. via
 * a compiler define) to return the appropriate handle.
 */
#ifndef VIRTIO_DMA_CONTEXT_GET_WDF_DMA_ENABLER
#define VIRTIO_DMA_CONTEXT_GET_WDF_DMA_ENABLER(_dmaCtx) VirtioDmaGetEnabler((_dmaCtx))
#endif

typedef struct _VIRTIO_DMA_SLOT {
    /* CPU virtual address. */
    _Field_size_bytes_(Size) VOID* Va;

    /* Device DMA/logical address. */
    UINT64 DmaAddress;

    /* Fixed usable size of this slot. */
    size_t Size;
} VIRTIO_DMA_SLOT;

NTSTATUS VirtioDmaPoolCreate(
    _In_ VIRTIO_DMA_CONTEXT* dma,
    _In_ size_t slotSize,
    _In_ size_t slotAlignment,
    _In_ ULONG slotCount,
    _In_ BOOLEAN cacheEnabled,
    _In_ WDFOBJECT parent,
    _Outptr_ VIRTIO_DMA_POOL** outPool);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS VirtioDmaPoolAlloc(_Inout_ VIRTIO_DMA_POOL* pool, _Out_ VIRTIO_DMA_SLOT* outSlot);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtioDmaPoolFree(_Inout_ VIRTIO_DMA_POOL* pool, _In_ const VIRTIO_DMA_SLOT* slot);

/*
 * Indirect descriptor tables must be physically contiguous because the device
 * reads them sequentially. Therefore they must come from this DMA pool or from
 * a WDFCOMMONBUFFER.
 */
struct virtq_desc;

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS VirtioIndirectTableInit(
    _In_ const VIRTIO_DMA_SLOT* slot,
    _In_ USHORT descCount,
    _Outptr_ struct virtq_desc** outTableVa,
    _Out_ UINT64* outTableDmaAddress);

#ifdef __cplusplus
} /* extern "C" */
#endif
