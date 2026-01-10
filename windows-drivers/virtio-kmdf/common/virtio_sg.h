#pragma once
/*
 * Virtio scatter-gather (SG) helpers for Windows 7 KMDF drivers.
 *
 * This module provides a mapping layer that converts Windows I/O buffers
 * (MDLs / WDFREQUEST buffers) into a scatter-gather list of DMA addresses
 * suitable for populating virtqueue descriptors.
 *
 * Design goals:
 *   - No allocations in the direct MDL->PFN mapping path (DISPATCH_LEVEL safe).
 *   - Optional WDF DMA transaction path for robust bus-address translation
 *     (IOMMU / DMA remapping aware) while keeping the transaction alive until
 *     the virtio device signals completion.
 *
 * Descriptor/queue sizing:
 *   - Virtio legacy/modern descriptor "len" is 32-bit, so callers must ensure
 *     the mapped byte length fits in 0xFFFFFFFF.
 *   - Callers should size their element storage using VirtioSgMaxElemsForMdl()
 *     and compare against queue capacity. If the resulting descriptor count is
 *     too high, drivers should prefer INDIRECT descriptors or fail the request.
 */

#include "virtio_dma.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct _VIRTIO_SG_ELEM {
    UINT64 Addr;
    ULONG Len;
    BOOLEAN DeviceWrite;
} VIRTIO_SG_ELEM;

typedef struct _VIRTIO_SG_LIST {
    VIRTIO_SG_ELEM* Elems;
    ULONG Count;
} VIRTIO_SG_LIST;

/*
 * Returns a worst-case upper bound on the number of SG elements required to
 * describe the requested byte range within an MDL chain. This is essentially
 * the number of pages spanned by the range (coalescing can reduce the actual
 * count).
 *
 * Returns 0 if the range is invalid.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
ULONG
VirtioSgMaxElemsForMdl(
    _In_ PMDL Mdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength
    );

/*
 * Builds an SG list from an MDL chain by walking the PFN array(s) and
 * generating per-page segments, coalescing physically-contiguous PFNs.
 *
 * Note: This "direct" path yields physical addresses (PFN<<PAGE_SHIFT) and does
 * not consult any DMA remapping/IOMMU. For production drivers that must obtain
 * true bus addresses, prefer the WDF DMA transaction path below.
 *
 * The resulting list is written into the caller-provided OutElems array.
 *
 * On success:
 *   - returns STATUS_SUCCESS
 *   - *OutCount is set to the number of elements written (<= OutCapacity)
 *
 * If OutCapacity is insufficient:
 *   - returns STATUS_BUFFER_TOO_SMALL
 *   - *OutCount is set to the number of elements required
 *   - OutElems contains the first OutCapacity elements (if OutElems!=NULL)
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
_Must_inspect_result_
NTSTATUS
VirtioSgBuildFromMdl(
    _In_ PMDL Mdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength,
    _In_ BOOLEAN DeviceWrite,
    _Out_writes_opt_(OutCapacity) VIRTIO_SG_ELEM* OutElems,
    _In_ ULONG OutCapacity,
    _Out_ ULONG* OutCount
    );

/*
 * WDF DMA transaction path
 * ------------------------
 *
 * This path uses WDFDMATRANSACTION to obtain bus addresses (SCATTER_GATHER_LIST)
 * and copies them into a VIRTIO_SG_ELEM array held by the mapping object.
 *
 * The transaction must remain alive until the virtio device signals completion
 * (used ring). Call VirtioWdfDmaCompleteAndRelease() at that point to finalize
 * the DMA transaction and release associated resources.
 *
 * VirtioWdfDmaStartMapping allocates WDF objects and (optionally) builds a
 * partial MDL chain. Callers should invoke it at <= APC_LEVEL.
 *
 * This helper is single-shot: it expects WDF to translate the entire buffer
 * range in one EvtProgramDma invocation. If the DMA adapter/framework must
 * split the buffer (max-length, max-SG elements, etc.), mapping will fail and
 * the caller should fall back to INDIRECT descriptors, a bounce buffer, or
 * otherwise segment the request.
 *
 * If EvtProgramDma is provided, it is invoked from the internal program-DMA
 * callback after the mapping object's Sg list has been populated. The callback
 * receives Context == VIRTIO_WDFDMA_MAPPING* (the same context passed to
 * WdfDmaTransactionExecute).
 */
/*
 * Dma parameter:
 *   Pass the VIRTIO_DMA_CONTEXT created by VirtioDmaCreate() (virtio_dma.h).
 */

typedef struct _VIRTIO_WDFDMA_MAPPING {
    /* WDF object that owns this mapping context. */
    WDFOBJECT Object;

    /* DMA transaction kept alive until virtio completion. */
    WDFDMATRANSACTION Transaction;
    BOOLEAN TransactionExecuted;
    BOOLEAN TransactionFinalized;

    /* Optional MDL chain created to represent a subrange of a larger buffer. */
    PMDL PartialMdlChain;

    /* Storage holding the SG elements (nonpaged). */
    WDFMEMORY ElemMemory;
    VIRTIO_SG_LIST Sg;
    ULONG SgCapacity;

    size_t ByteLength;
    EVT_WDF_PROGRAM_DMA* UserEvtProgramDma;
} VIRTIO_WDFDMA_MAPPING;

_IRQL_requires_max_(APC_LEVEL)
_Must_inspect_result_
NTSTATUS
VirtioWdfDmaStartMapping(
    _In_ VIRTIO_DMA_CONTEXT* Dma,
    _In_opt_ WDFREQUEST RequestOrNull,
    _In_opt_ PMDL Mdl,
    _In_ size_t Offset,
    _In_ size_t Length,
    _In_ WDF_DMA_DIRECTION Direction,
    _In_opt_ EVT_WDF_PROGRAM_DMA* EvtProgramDma,
    _In_ WDFOBJECT Parent,
    _Out_ VIRTIO_WDFDMA_MAPPING** OutMapping
    );

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioWdfDmaCompleteAndRelease(
    _In_ VIRTIO_WDFDMA_MAPPING* Mapping
    );

#if DBG
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioSgDebugDumpList(
    _In_reads_(Count) const VIRTIO_SG_ELEM* Elems,
    _In_ ULONG Count,
    _In_opt_ PCSTR Prefix
    );

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioSgDebugDumpMdl(
    _In_ PMDL Mdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength,
    _In_ BOOLEAN DeviceWrite
    );
#endif

#ifdef __cplusplus
} // extern "C"
#endif
