#include "virtqueue_ring.h"

static __forceinline SIZE_T
VirtqueueRingAlignUp(
    _In_ SIZE_T Value,
    _In_ SIZE_T Alignment)
{
    NT_ASSERT(Alignment != 0);
    NT_ASSERT((Alignment & (Alignment - 1)) == 0); /* power-of-two */
    return (Value + (Alignment - 1)) & ~(Alignment - 1);
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
    _Out_ VIRTQUEUE_RING_LAYOUT* Layout)
{
    SIZE_T descOffset;
    SIZE_T descSize;
    SIZE_T availSize;
    SIZE_T usedSize;
    SIZE_T availOffset;
    SIZE_T usedOffset;
    SIZE_T totalSize;

    if (Layout == NULL || QueueSize == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Split ring sizes (virtio spec):
     *   desc  = 16 * queueSize
     *   avail = 4 + (2 * queueSize) + (eventIdx ? 2 : 0)
     *   used  = 4 + (8 * queueSize) + (eventIdx ? 2 : 0)
     */
    descOffset = VirtqueueRingAlignUp(0, 16);
    descSize = sizeof(struct virtq_desc) * (SIZE_T)QueueSize;
    availSize = sizeof(UINT16) * (SIZE_T)(2 + QueueSize + (EventIdxEnabled ? 1 : 0));
    usedSize =
        (sizeof(UINT16) * 2) + (sizeof(struct virtq_used_elem) * (SIZE_T)QueueSize) +
        (EventIdxEnabled ? sizeof(UINT16) : 0);

    availOffset = VirtqueueRingAlignUp(descOffset + descSize, 2);
    usedOffset = VirtqueueRingAlignUp(availOffset + availSize, 4);
    totalSize = usedOffset + usedSize;

    Layout->DescSize = descSize;
    Layout->AvailSize = availSize;
    Layout->UsedSize = usedSize;

    Layout->DescOffset = descOffset;
    Layout->AvailOffset = availOffset;
    Layout->UsedOffset = usedOffset;
    Layout->TotalSize = totalSize;

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
        !VirtqueueRingIsAligned64(Ring->UsedDma, 4)) {
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

    PAGED_CODE();

    if (DmaCtx == NULL || Ring == NULL || QueueSize == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Ring, sizeof(*Ring));
    RtlZeroMemory(&layout, sizeof(layout));

    status = VirtqueueRingLayoutCompute(QueueSize, EventIdxEnabled, &layout);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Prefer page alignment but accept 16-byte alignment as a minimum. */
    status = VirtqueueRingDmaAllocCommonBuffer(DmaCtx, ParentObject, layout.TotalSize, PAGE_SIZE, &Ring->CommonBuffer);
    if (!NT_SUCCESS(status)) {
        status = VirtqueueRingDmaAllocCommonBuffer(DmaCtx, ParentObject, layout.TotalSize, 16, &Ring->CommonBuffer);
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

    ASSERT(VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Desc, 16));
    ASSERT(VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Avail, 2));
    ASSERT(VirtqueueRingIsAligned64((UINT64)(ULONG_PTR)Ring->Used, 4));

    ASSERT(VirtqueueRingIsAligned64(Ring->DescDma, 16));
    ASSERT(VirtqueueRingIsAligned64(Ring->AvailDma, 2));
    ASSERT(VirtqueueRingIsAligned64(Ring->UsedDma, 4));
}
#endif
