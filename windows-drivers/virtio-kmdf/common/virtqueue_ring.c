#include "virtqueue_ring.h"

#include <ntintsafe.h>

static __forceinline _Must_inspect_result_ NTSTATUS
VirtqueueRingAlignUp(
    _In_ SIZE_T Value,
    _In_ SIZE_T Alignment,
    _Out_ SIZE_T* OutAligned)
{
    NTSTATUS status;
    SIZE_T tmp;

    if (OutAligned == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    NT_ASSERT(Alignment != 0);
    NT_ASSERT((Alignment & (Alignment - 1)) == 0); /* power-of-two */

    status = RtlSizeTAdd(Value, Alignment - 1, &tmp);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    *OutAligned = tmp & ~(Alignment - 1);
    return STATUS_SUCCESS;
}

static __forceinline BOOLEAN
VirtqueueRingIsAligned64(
    _In_ UINT64 Value,
    _In_ UINT64 Alignment)
{
    return (Value & (Alignment - 1)) == 0;
}

NTSTATUS
VirtqueueRingLayoutCompute(
    _In_ USHORT QueueSize,
    _In_ BOOLEAN EventIdxEnabled,
    _In_ SIZE_T RingAlignment,
    _Out_ VIRTQUEUE_RING_LAYOUT* Layout)
{
    NTSTATUS status;
    SIZE_T descOffset;
    SIZE_T descSize;
    SIZE_T availSize;
    SIZE_T usedSize;
    SIZE_T availOffset;
    SIZE_T usedOffset;
    SIZE_T totalSize;
    SIZE_T tmp;
    SIZE_T descEnd;
    SIZE_T availEnd;

    if (Layout == NULL || QueueSize == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (RingAlignment < 4 || (RingAlignment & (RingAlignment - 1)) != 0) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(Layout, sizeof(*Layout));

    /*
     * Split ring sizes (virtio spec):
     *   desc  = 16 * queueSize
     *   avail = 4 + (2 * queueSize) + (eventIdx ? 2 : 0)
     *   used  = 4 + (8 * queueSize) + (eventIdx ? 2 : 0)
     */
    status = VirtqueueRingAlignUp(0, 16, &descOffset);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = RtlSizeTMult(sizeof(struct virtq_desc), (SIZE_T)QueueSize, &descSize);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = RtlSizeTMult(sizeof(UINT16), (SIZE_T)(2 + QueueSize + (EventIdxEnabled ? 1 : 0)), &availSize);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    usedSize = sizeof(UINT16) * 2;
    status = RtlSizeTMult(sizeof(struct virtq_used_elem), (SIZE_T)QueueSize, &tmp);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = RtlSizeTAdd(usedSize, tmp, &usedSize);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (EventIdxEnabled) {
        status = RtlSizeTAdd(usedSize, sizeof(UINT16), &usedSize);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    status = RtlSizeTAdd(descOffset, descSize, &descEnd);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = VirtqueueRingAlignUp(descEnd, 2, &availOffset);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = RtlSizeTAdd(availOffset, availSize, &availEnd);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = VirtqueueRingAlignUp(availEnd, RingAlignment, &usedOffset);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = RtlSizeTAdd(usedOffset, usedSize, &totalSize);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    Layout->DescSize = descSize;
    Layout->AvailSize = availSize;
    Layout->UsedSize = usedSize;

    Layout->DescOffset = descOffset;
    Layout->AvailOffset = availOffset;
    Layout->UsedOffset = usedOffset;
    Layout->TotalSize = totalSize;

    NT_ASSERT((Layout->DescOffset & (16 - 1)) == 0);
    NT_ASSERT((Layout->AvailOffset & (2 - 1)) == 0);
    NT_ASSERT((Layout->UsedOffset & (4 - 1)) == 0);

    NT_ASSERT(Layout->DescOffset + Layout->DescSize <= Layout->AvailOffset);
    NT_ASSERT(Layout->AvailOffset + Layout->AvailSize <= Layout->UsedOffset);
    NT_ASSERT(Layout->UsedOffset + Layout->UsedSize == Layout->TotalSize);

    return STATUS_SUCCESS;
}

_Must_inspect_result_
static NTSTATUS
VirtqueueRingDmaAllocCommonBuffer(
    _In_ VIRTIO_DMA_CONTEXT* DmaCtx,
    _In_opt_ WDFOBJECT ParentObject,
    _In_ size_t Length,
    _In_ size_t Alignment,
    _Out_ VIRTIO_COMMON_BUFFER* OutBuffer)
{
    if (ParentObject != NULL) {
        return VirtioDmaAllocCommonBufferWithParent(DmaCtx, Length, Alignment, FALSE, ParentObject, OutBuffer);
    }

    return VirtioDmaAllocCommonBuffer(DmaCtx, Length, Alignment, FALSE, OutBuffer);
}

_Must_inspect_result_
static NTSTATUS
VirtqueueRingDmaValidateAlignment(
    _In_ const VIRTQUEUE_RING_DMA* Ring)
{
    if (!VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Desc, 16) ||
        !VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Avail, 2) ||
        !VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Used, 4)) {
        return STATUS_DATATYPE_MISALIGNMENT;
    }

    if (!VirtqueueRingIsAligned64(Ring->DescDma, 16) ||
        !VirtqueueRingIsAligned64(Ring->AvailDma, 2) ||
        !VirtqueueRingIsAligned64(Ring->UsedDma, (UINT64)Ring->RingAlignment)) {
        return STATUS_DATATYPE_MISALIGNMENT;
    }

    return STATUS_SUCCESS;
}

NTSTATUS
VirtqueueRingDmaAlloc(
    _In_ VIRTIO_DMA_CONTEXT* DmaCtx,
    _In_opt_ WDFOBJECT ParentObject,
    _In_ USHORT QueueSize,
    _In_ BOOLEAN EventIdxEnabled,
    _Out_ VIRTQUEUE_RING_DMA* Ring)
{
    NTSTATUS status;
    VIRTQUEUE_RING_LAYOUT layout;
    UINT64 baseDma;
    PUCHAR baseVa;
    SIZE_T ringAlign;

    PAGED_CODE();

    if (DmaCtx == NULL || Ring == NULL || QueueSize == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Ring, sizeof(*Ring));
    RtlZeroMemory(&layout, sizeof(layout));

    /*
     * Prefer PAGE_SIZE ring alignment for legacy virtio-pci, but fall back to 16
     * if the platform/DMA constraints can't satisfy that requirement.
     */
    ringAlign = PAGE_SIZE;
    status = VirtqueueRingLayoutCompute(QueueSize, EventIdxEnabled, ringAlign, &layout);
    if (NT_SUCCESS(status)) {
        status = VirtqueueRingDmaAllocCommonBuffer(DmaCtx, ParentObject, layout.TotalSize, ringAlign, &Ring->CommonBuffer);
    }
    if (!NT_SUCCESS(status)) {
        ringAlign = 16;
        status = VirtqueueRingLayoutCompute(QueueSize, EventIdxEnabled, ringAlign, &layout);
        if (NT_SUCCESS(status)) {
            status = VirtqueueRingDmaAllocCommonBuffer(DmaCtx, ParentObject, layout.TotalSize, ringAlign, &Ring->CommonBuffer);
        }
    }
    if (!NT_SUCCESS(status)) {
        RtlZeroMemory(Ring, sizeof(*Ring));
        return status;
    }

    RtlZeroMemory(Ring->CommonBuffer.Va, layout.TotalSize);

    baseVa = (PUCHAR)Ring->CommonBuffer.Va;
    baseDma = Ring->CommonBuffer.Dma;

    Ring->Desc = (volatile struct virtq_desc*)(baseVa + layout.DescOffset);
    Ring->Avail = (volatile struct virtq_avail*)(baseVa + layout.AvailOffset);
    Ring->Used = (volatile struct virtq_used*)(baseVa + layout.UsedOffset);

    Ring->DescDma = baseDma + (UINT64)layout.DescOffset;
    Ring->AvailDma = baseDma + (UINT64)layout.AvailOffset;
    Ring->UsedDma = baseDma + (UINT64)layout.UsedOffset;

    Ring->QueueSize = QueueSize;
    Ring->RingAlignment = ringAlign;

    status = VirtqueueRingDmaValidateAlignment(Ring);
    if (!NT_SUCCESS(status)) {
        VirtqueueRingDmaFree(Ring);
        return status;
    }

#if DBG
    VirtqueueRingDmaSelfTest(Ring);
#endif

    return STATUS_SUCCESS;
}

VOID
VirtqueueRingDmaFree(
    _Inout_ VIRTQUEUE_RING_DMA* Ring)
{
    PAGED_CODE();

    if (Ring == NULL) {
        return;
    }

    VirtioDmaFreeCommonBuffer(&Ring->CommonBuffer);
    RtlZeroMemory(Ring, sizeof(*Ring));
}

#if DBG
VOID
VirtqueueRingDmaSelfTest(
    _In_ const VIRTQUEUE_RING_DMA* Ring)
{
    ASSERT(Ring != NULL);
    ASSERT(Ring->QueueSize != 0);
    ASSERT(Ring->RingAlignment != 0);

    ASSERT(VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Desc, 16));
    ASSERT(VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Avail, 2));
    ASSERT(VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Used, 4));

    ASSERT(VirtqueueRingIsAligned64(Ring->DescDma, 16));
    ASSERT(VirtqueueRingIsAligned64(Ring->AvailDma, 2));
    ASSERT(VirtqueueRingIsAligned64(Ring->UsedDma, (UINT64)Ring->RingAlignment));
}
#endif
