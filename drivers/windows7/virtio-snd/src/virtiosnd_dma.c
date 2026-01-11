/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd_dma.h"

static __forceinline MEMORY_CACHING_TYPE
VirtIoSndCacheTypeFromBool(_In_ BOOLEAN CacheEnabled)
{
    return CacheEnabled ? MmCached : MmNonCached;
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndDmaInit(PDEVICE_OBJECT PhysicalDeviceObject, PVIRTIOSND_DMA_CONTEXT Ctx)
{
    DEVICE_DESCRIPTION desc;
    ULONG mapRegs;
    PDMA_ADAPTER adapter;

    if (Ctx == NULL || PhysicalDeviceObject == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Ctx, sizeof(*Ctx));
    Ctx->RingCacheEnabled = FALSE;

    RtlZeroMemory(&desc, sizeof(desc));
    desc.Version = DEVICE_DESCRIPTION_VERSION;
    desc.Master = TRUE;
    desc.ScatterGather = TRUE;
    desc.Dma32BitAddresses = FALSE; /* allow >4GiB */
    desc.InterfaceType = PCIBus;
    desc.BusNumber = 0;
    desc.MaximumLength = 0xFFFFFFFFu;

    mapRegs = 0;
    adapter = IoGetDmaAdapter(PhysicalDeviceObject, &desc, &mapRegs);
    if (adapter == NULL) {
        VIRTIOSND_TRACE_ERROR("IoGetDmaAdapter returned NULL; falling back to MmAllocateContiguousMemory\n");
        return STATUS_SUCCESS;
    }

    if (adapter->DmaOperations == NULL ||
        adapter->DmaOperations->AllocateCommonBuffer == NULL ||
        adapter->DmaOperations->FreeCommonBuffer == NULL) {
        if (adapter->DmaOperations != NULL && adapter->DmaOperations->PutDmaAdapter != NULL) {
            adapter->DmaOperations->PutDmaAdapter(adapter);
        }

        VIRTIOSND_TRACE_ERROR("DMA adapter missing common buffer ops; falling back to MmAllocateContiguousMemory\n");
        return STATUS_SUCCESS;
    }

    Ctx->Adapter = adapter;
    Ctx->MapRegisters = mapRegs;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
VirtIoSndDmaUninit(PVIRTIOSND_DMA_CONTEXT Ctx)
{
    PDMA_ADAPTER adapter;

    if (Ctx == NULL) {
        return;
    }

    adapter = Ctx->Adapter;
    Ctx->Adapter = NULL;
    Ctx->MapRegisters = 0;
    Ctx->RingCacheEnabled = FALSE;

    if (adapter != NULL && adapter->DmaOperations != NULL && adapter->DmaOperations->PutDmaAdapter != NULL) {
        adapter->DmaOperations->PutDmaAdapter(adapter);
    }
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndAllocCommonBuffer(PVIRTIOSND_DMA_CONTEXT Ctx, SIZE_T Size, BOOLEAN CacheEnabled, PVIRTIOSND_DMA_BUFFER Out)
{
    if (Out != NULL) {
        RtlZeroMemory(Out, sizeof(*Out));
    }

    if (Ctx == NULL || Out == NULL || Size == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Prefer adapter-aware common buffers so the returned DmaAddr is a device DMA
     * (logical/bus) address suitable for programming into virtio queue regs.
     */
    if (Ctx->Adapter != NULL &&
        Ctx->Adapter->DmaOperations != NULL &&
        Ctx->Adapter->DmaOperations->AllocateCommonBuffer != NULL) {
        PHYSICAL_ADDRESS logical;
        PVOID va;
        BOOLEAN cacheEnabled;

        if (Size > MAXULONG) {
            return STATUS_INVALID_PARAMETER;
        }

        cacheEnabled = CacheEnabled;

        logical.QuadPart = 0;
        va = Ctx->Adapter->DmaOperations->AllocateCommonBuffer(Ctx->Adapter, (ULONG)Size, &logical, cacheEnabled);
        if (va == NULL && !cacheEnabled) {
            /*
             * Best-effort fallback: cached common buffer. This is still correct on
             * x86/x64 (cache-coherent DMA) and avoids hard failure if the DMA
             * framework cannot satisfy a non-cached request.
             */
            cacheEnabled = TRUE;
            logical.QuadPart = 0;
            va = Ctx->Adapter->DmaOperations->AllocateCommonBuffer(Ctx->Adapter, (ULONG)Size, &logical, cacheEnabled);
        }
        if (va == NULL) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        if (cacheEnabled != CacheEnabled) {
            VIRTIOSND_TRACE(
                "DMA: AllocateCommonBuffer non-cached failed; using cached buffer %Iu bytes VA=%p DMA=%I64x\n",
                Size,
                va,
                (ULONGLONG)logical.QuadPart);
        }

        Out->Va = va;
        Out->DmaAddr = (UINT64)logical.QuadPart;
        Out->Size = Size;
        Out->IsCommonBuffer = TRUE;
        Out->CacheEnabled = cacheEnabled;
        return STATUS_SUCCESS;
    }

    /* Fallback: contiguous allocation + CPU physical address (not IOMMU-safe). */
    {
        PHYSICAL_ADDRESS low;
        PHYSICAL_ADDRESS high;
        PHYSICAL_ADDRESS boundary;
        PVOID va;
        PHYSICAL_ADDRESS pa;
        MEMORY_CACHING_TYPE cacheType;
        BOOLEAN cacheEnabled;

        low.QuadPart = 0;
        high.QuadPart = -1;
        boundary.QuadPart = 0;
        cacheEnabled = CacheEnabled;
        cacheType = VirtIoSndCacheTypeFromBool(cacheEnabled);

        va = MmAllocateContiguousMemorySpecifyCache(Size, low, high, boundary, cacheType);
        if (va == NULL && !cacheEnabled) {
            /*
             * Best-effort fallback: cached contiguous allocation. This is still
             * correct on x86/x64 (cache-coherent DMA) and avoids hard failure if
             * the non-cached pool is fragmented.
             */
            cacheEnabled = TRUE;
            cacheType = MmCached;
            va = MmAllocateContiguousMemorySpecifyCache(Size, low, high, boundary, cacheType);
        }
        if (va == NULL) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        pa = MmGetPhysicalAddress(va);

        VIRTIOSND_TRACE(
            "DMA: alloc contiguous buffer %Iu bytes cache=%s VA=%p DMA=%I64x\n",
            Size,
            cacheEnabled ? "MmCached" : "MmNonCached",
            va,
            (ULONGLONG)pa.QuadPart);

        Out->Va = va;
        Out->DmaAddr = (UINT64)pa.QuadPart;
        Out->Size = Size;
        Out->IsCommonBuffer = FALSE;
        Out->CacheEnabled = cacheEnabled;
        return STATUS_SUCCESS;
    }
}

_Use_decl_annotations_
VOID
VirtIoSndFreeCommonBuffer(PVIRTIOSND_DMA_CONTEXT Ctx, PVIRTIOSND_DMA_BUFFER Buf)
{
    if (Buf == NULL || Buf->Va == NULL || Buf->Size == 0) {
        return;
    }

    if (Buf->IsCommonBuffer) {
        PHYSICAL_ADDRESS logical;

        if (Ctx == NULL || Ctx->Adapter == NULL ||
            Ctx->Adapter->DmaOperations == NULL ||
            Ctx->Adapter->DmaOperations->FreeCommonBuffer == NULL) {
            ASSERT(FALSE);
            return;
        }

        ASSERT(Buf->Size <= MAXULONG);
        logical.QuadPart = (LONGLONG)Buf->DmaAddr;
        Ctx->Adapter->DmaOperations->FreeCommonBuffer(
            Ctx->Adapter,
            (ULONG)Buf->Size,
            logical,
            Buf->Va,
            Buf->CacheEnabled);
    } else {
        MEMORY_CACHING_TYPE cacheType;
        cacheType = VirtIoSndCacheTypeFromBool(Buf->CacheEnabled);
        MmFreeContiguousMemorySpecifyCache(Buf->Va, Buf->Size, cacheType);
    }

    RtlZeroMemory(Buf, sizeof(*Buf));
}
