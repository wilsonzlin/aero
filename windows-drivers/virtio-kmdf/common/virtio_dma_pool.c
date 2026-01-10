/*
 * DMA-safe pool of small, per-request buffers for virtio KMDF drivers.
 *
 * See virtio_dma_pool.h for design notes.
 */

#include "virtio_dma_pool.h"

#define VIRTIO_DMA_POOL_TAG 'pDmV' /* "VmDp" */

#ifndef NonPagedPoolNx
#define NonPagedPoolNx NonPagedPool
#endif

/*
 * virtq_desc is a 16-byte structure per the virtio spec.
 * We avoid a hard dependency on the virtio headers here.
 */
#define VIRTIO_VIRTQ_DESC_BYTES 16u

static __forceinline BOOLEAN VirtioIsPowerOfTwoSizeT(_In_ size_t value)
{
    return (value != 0) && ((value & (value - 1)) == 0);
}

static __forceinline size_t VirtioAlignUpSizeT(_In_ size_t value, _In_ size_t alignment)
{
    NT_ASSERT(alignment != 0);
    NT_ASSERT(VirtioIsPowerOfTwoSizeT(alignment));
    return (value + (alignment - 1)) & ~(alignment - 1);
}

static __forceinline UINT64 VirtioAlignUpU64(_In_ UINT64 value, _In_ size_t alignment)
{
    NT_ASSERT(alignment != 0);
    NT_ASSERT(VirtioIsPowerOfTwoSizeT(alignment));
    return (value + (alignment - 1)) & ~((UINT64)alignment - 1);
}

typedef struct _VIRTIO_DMA_POOL {
    WDFCOMMONBUFFER CommonBuffer;
    size_t CommonBufferLength;

    PUCHAR BaseVa;
    UINT64 BaseDmaAddress;
    size_t BaseOffset;

    size_t SlotSize;
    size_t SlotStride;
    size_t SlotAlignment;
    ULONG SlotCount;
    size_t PoolBytes;

    WDFSPINLOCK Lock;

    RTL_BITMAP AllocationBitmap;
    PULONG AllocationBitmapBuffer;
    ULONG AllocationBitmapBufferUlongs;

    ULONG OutstandingAllocations;
} VIRTIO_DMA_POOL;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_DMA_POOL, VirtioDmaPoolGetContext);

static VOID VirtioDmaPoolEvtCleanup(_In_ WDFOBJECT object)
{
    VIRTIO_DMA_POOL* pool = VirtioDmaPoolGetContext(object);

#if DBG
    if (pool->AllocationBitmapBuffer != NULL) {
        NT_ASSERT(pool->OutstandingAllocations == 0);
        NT_ASSERT(RtlNumberOfSetBits(&pool->AllocationBitmap) == 0);
    }
#endif

    if (pool->AllocationBitmapBuffer != NULL) {
        ExFreePoolWithTag(pool->AllocationBitmapBuffer, VIRTIO_DMA_POOL_TAG);
        pool->AllocationBitmapBuffer = NULL;
        pool->AllocationBitmapBufferUlongs = 0;
    }
}

NTSTATUS VirtioDmaPoolCreate(
    _In_ VIRTIO_DMA_CONTEXT* dma,
    _In_ size_t slotSize,
    _In_ size_t slotAlignment,
    _In_ ULONG slotCount,
    _In_ BOOLEAN cacheEnabled,
    _In_ WDFOBJECT parent,
    _Outptr_ VIRTIO_DMA_POOL** outPool)
{
    NTSTATUS status;
    WDFOBJECT poolObject = NULL;
    VIRTIO_DMA_POOL* pool;
    WDFDMAENABLER dmaEnabler;
    size_t slotStride;
    size_t poolBytes;
    size_t commonBufferLength;
    size_t bitmapUlongs;
    size_t bitmapBytes;

    if (outPool == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *outPool = NULL;

    if ((dma == NULL) || (slotSize == 0) || (slotCount == 0) || (parent == NULL)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (slotAlignment == 0) {
        slotAlignment = 1;
    }

    if (!VirtioIsPowerOfTwoSizeT(slotAlignment)) {
        return STATUS_INVALID_PARAMETER;
    }

    slotStride = VirtioAlignUpSizeT(slotSize, slotAlignment);

    status = RtlSizeTMult(slotStride, (size_t)slotCount, &poolBytes);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /*
     * Over-allocate up to (slotAlignment-1) bytes so we can align the first slot
     * start to slotAlignment even if WdfCommonBufferGetAlignedLogicalAddress()
     * does not meet our per-slot alignment requirement.
     */
    status = RtlSizeTAdd(poolBytes, slotAlignment - 1, &commonBufferLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, VIRTIO_DMA_POOL);
    attributes.ParentObject = parent;
    attributes.ExecutionLevel = WdfExecutionLevelDispatch;
    attributes.SynchronizationScope = WdfSynchronizationScopeNone;
    attributes.EvtCleanupCallback = VirtioDmaPoolEvtCleanup;

    status = WdfObjectCreate(&attributes, &poolObject);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    pool = VirtioDmaPoolGetContext(poolObject);
    RtlZeroMemory(pool, sizeof(*pool));

    pool->SlotSize = slotSize;
    pool->SlotStride = slotStride;
    pool->SlotAlignment = slotAlignment;
    pool->SlotCount = slotCount;
    pool->PoolBytes = poolBytes;

    WDF_OBJECT_ATTRIBUTES lockAttributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
    lockAttributes.ParentObject = poolObject;
    lockAttributes.ExecutionLevel = WdfExecutionLevelDispatch;
    lockAttributes.SynchronizationScope = WdfSynchronizationScopeNone;

    status = WdfSpinLockCreate(&lockAttributes, &pool->Lock);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(poolObject);
        return status;
    }

    /* Allocation bitmap is variable-sized, so allocate it separately. */
    bitmapUlongs = (slotCount + ((sizeof(ULONG) * 8) - 1)) / (sizeof(ULONG) * 8);
    bitmapBytes = bitmapUlongs * sizeof(ULONG);
    if (bitmapBytes == 0) {
        WdfObjectDelete(poolObject);
        return STATUS_INVALID_PARAMETER;
    }

    pool->AllocationBitmapBuffer = (PULONG)ExAllocatePoolWithTag(NonPagedPoolNx, bitmapBytes, VIRTIO_DMA_POOL_TAG);
    if (pool->AllocationBitmapBuffer == NULL) {
        WdfObjectDelete(poolObject);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    pool->AllocationBitmapBufferUlongs = (ULONG)bitmapUlongs;
    RtlZeroMemory(pool->AllocationBitmapBuffer, bitmapBytes);
    RtlInitializeBitMap(&pool->AllocationBitmap, pool->AllocationBitmapBuffer, slotCount);

    dmaEnabler = VIRTIO_DMA_CONTEXT_GET_WDF_DMA_ENABLER(dma);
    if (dmaEnabler == NULL) {
        WdfObjectDelete(poolObject);
        return STATUS_INVALID_PARAMETER;
    }

    WDF_COMMON_BUFFER_CONFIG commonBufferConfig;
    WDF_COMMON_BUFFER_CONFIG_INIT(&commonBufferConfig);
    commonBufferConfig.CacheEnabled = cacheEnabled;

    WDF_OBJECT_ATTRIBUTES commonBufferAttributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&commonBufferAttributes);
    commonBufferAttributes.ParentObject = poolObject;
    commonBufferAttributes.ExecutionLevel = WdfExecutionLevelDispatch;
    commonBufferAttributes.SynchronizationScope = WdfSynchronizationScopeNone;

    status = WdfCommonBufferCreateWithConfig(
        dmaEnabler, commonBufferLength, &commonBufferConfig, &commonBufferAttributes, &pool->CommonBuffer);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(poolObject);
        return status;
    }

    pool->CommonBufferLength = WdfCommonBufferGetLength(pool->CommonBuffer);
    {
        const UINT64 rawDmaAddress = WdfCommonBufferGetAlignedLogicalAddress(pool->CommonBuffer).QuadPart;
        PUCHAR rawVa = (PUCHAR)WdfCommonBufferGetAlignedVirtualAddress(pool->CommonBuffer);

        const UINT64 alignedDmaAddress = VirtioAlignUpU64(rawDmaAddress, slotAlignment);
        const size_t baseOffset = (size_t)(alignedDmaAddress - rawDmaAddress);

        pool->BaseOffset = baseOffset;
        pool->BaseDmaAddress = alignedDmaAddress;
        pool->BaseVa = rawVa + baseOffset;
    }

#if DBG
    NT_ASSERT(pool->BaseOffset < slotAlignment);
    NT_ASSERT((pool->BaseDmaAddress & (slotAlignment - 1)) == 0);
    NT_ASSERT((((ULONG_PTR)pool->BaseVa) & (slotAlignment - 1)) == 0);
    NT_ASSERT((pool->SlotStride & (slotAlignment - 1)) == 0);
    NT_ASSERT(pool->SlotSize <= pool->SlotStride);
#endif

    /* Verify that the aligned base + pool range fits inside the common buffer. */
    if ((pool->BaseOffset > pool->CommonBufferLength) ||
        (pool->PoolBytes > (pool->CommonBufferLength - pool->BaseOffset))) {
        WdfObjectDelete(poolObject);
        return STATUS_BUFFER_TOO_SMALL;
    }

    /* Optional: zero the pool's usable area to avoid leaking stale memory. */
    RtlZeroMemory(pool->BaseVa, pool->PoolBytes);

    *outPool = pool;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS VirtioDmaPoolAlloc(_Inout_ VIRTIO_DMA_POOL* pool, _Out_ VIRTIO_DMA_SLOT* outSlot)
{
    ULONG bitIndex;
    size_t slotOffset;

    if ((pool == NULL) || (outSlot == NULL)) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(outSlot, sizeof(*outSlot));

    WdfSpinLockAcquire(pool->Lock);

    bitIndex = RtlFindClearBitsAndSet(&pool->AllocationBitmap, 1, 0);
    if (bitIndex == 0xFFFFFFFFu) {
        WdfSpinLockRelease(pool->Lock);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    pool->OutstandingAllocations++;

#if DBG
    NT_ASSERT(bitIndex < pool->SlotCount);
    NT_ASSERT(RtlTestBit(&pool->AllocationBitmap, bitIndex));
    NT_ASSERT(RtlNumberOfSetBits(&pool->AllocationBitmap) == pool->OutstandingAllocations);
#endif

    WdfSpinLockRelease(pool->Lock);

    if (!NT_SUCCESS(RtlSizeTMult(pool->SlotStride, (size_t)bitIndex, &slotOffset))) {
        /*
         * Should be unreachable because bitIndex < SlotCount and we already
         * computed PoolBytes = SlotStride * SlotCount in Create.
         */
        NT_ASSERT(FALSE);
        return STATUS_INTEGER_OVERFLOW;
    }

    outSlot->Va = pool->BaseVa + slotOffset;
    outSlot->DmaAddress = pool->BaseDmaAddress + (UINT64)slotOffset;
    outSlot->Size = pool->SlotSize;

#if DBG
    NT_ASSERT((outSlot->DmaAddress & (pool->SlotAlignment - 1)) == 0);
    NT_ASSERT((((ULONG_PTR)outSlot->Va) & (pool->SlotAlignment - 1)) == 0);
    NT_ASSERT(slotOffset + pool->SlotSize <= pool->PoolBytes);
#endif

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtioDmaPoolFree(_Inout_ VIRTIO_DMA_POOL* pool, _In_ const VIRTIO_DMA_SLOT* slot)
{
    size_t slotOffset;
    ULONG bitIndex;

    if ((pool == NULL) || (slot == NULL) || (slot->Va == NULL)) {
        NT_ASSERT(FALSE);
        return;
    }

    if ((PUCHAR)slot->Va < pool->BaseVa) {
        NT_ASSERT(FALSE);
        return;
    }

    slotOffset = (size_t)((PUCHAR)slot->Va - pool->BaseVa);

    if ((slotOffset >= pool->PoolBytes) || ((slotOffset % pool->SlotStride) != 0)) {
        NT_ASSERT(FALSE);
        return;
    }

    bitIndex = (ULONG)(slotOffset / pool->SlotStride);

#if DBG
    NT_ASSERT(bitIndex < pool->SlotCount);
    NT_ASSERT(slot->DmaAddress == (pool->BaseDmaAddress + (UINT64)slotOffset));
    NT_ASSERT(slotOffset + pool->SlotSize <= pool->PoolBytes);
#endif

    WdfSpinLockAcquire(pool->Lock);

#if DBG
    NT_ASSERT(RtlTestBit(&pool->AllocationBitmap, bitIndex));
#endif

    RtlClearBit(&pool->AllocationBitmap, bitIndex);

    if (pool->OutstandingAllocations == 0) {
        NT_ASSERT(FALSE);
    } else {
        pool->OutstandingAllocations--;
    }

#if DBG
    NT_ASSERT(RtlNumberOfSetBits(&pool->AllocationBitmap) == pool->OutstandingAllocations);
#endif

    WdfSpinLockRelease(pool->Lock);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS VirtioIndirectTableInit(
    _In_ const VIRTIO_DMA_SLOT* slot,
    _In_ USHORT descCount,
    _Outptr_ struct virtq_desc** outTableVa,
    _Out_ UINT64* outTableDmaAddress)
{
    NTSTATUS status;
    size_t requiredBytes;

    if ((slot == NULL) || (outTableVa == NULL) || (outTableDmaAddress == NULL) || (descCount == 0)) {
        return STATUS_INVALID_PARAMETER;
    }

    status = RtlSizeTMult((size_t)descCount, (size_t)VIRTIO_VIRTQ_DESC_BYTES, &requiredBytes);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (slot->Size < requiredBytes) {
        return STATUS_BUFFER_TOO_SMALL;
    }

#if DBG
    NT_ASSERT((slot->DmaAddress & (VIRTIO_VIRTQ_DESC_BYTES - 1)) == 0);
    NT_ASSERT((((ULONG_PTR)slot->Va) & (VIRTIO_VIRTQ_DESC_BYTES - 1)) == 0);
#endif

    *outTableVa = (struct virtq_desc*)slot->Va;
    *outTableDmaAddress = slot->DmaAddress;
    return STATUS_SUCCESS;
}
