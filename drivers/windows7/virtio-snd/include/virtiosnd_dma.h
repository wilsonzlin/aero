/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

/*
 * virtiosnd_dma: WDM-only DMA/common-buffer helpers.
 *
 * Virtio queue configuration registers (virtio-pci modern: queue_desc/avail/used)
 * must be programmed with *device DMA addresses* (logical/bus addresses), not
 * CPU physical addresses. MmGetPhysicalAddress is not DMA-adapter/IOMMU aware.
 *
 * This module prefers IoGetDmaAdapter + AllocateCommonBuffer to obtain a DMA
 * address that is valid for the device. If an adapter is not available, it
 * falls back to MmAllocateContiguousMemorySpecifyCache and uses the physical
 * address as a best-effort DMA address (sufficient for the QEMU/Aero
 * environment, but not guaranteed on IOMMU systems).
 */

typedef struct _VIRTIOSND_DMA_BUFFER {
    PVOID Va;
    UINT64 DmaAddr;
    SIZE_T Size;
    BOOLEAN IsCommonBuffer;
    BOOLEAN CacheEnabled;
} VIRTIOSND_DMA_BUFFER, *PVIRTIOSND_DMA_BUFFER;

typedef struct _VIRTIOSND_DMA_CONTEXT {
    PDMA_ADAPTER Adapter;
    ULONG MapRegisters;
    BOOLEAN RingCacheEnabled;
} VIRTIOSND_DMA_CONTEXT, *PVIRTIOSND_DMA_CONTEXT;

_Must_inspect_result_
NTSTATUS VirtIoSndDmaInit(_In_ PDEVICE_OBJECT PhysicalDeviceObject, _Out_ PVIRTIOSND_DMA_CONTEXT Ctx);

VOID VirtIoSndDmaUninit(_Inout_ PVIRTIOSND_DMA_CONTEXT Ctx);

_Must_inspect_result_
NTSTATUS VirtIoSndAllocCommonBuffer(
    _In_ PVIRTIOSND_DMA_CONTEXT Ctx,
    _In_ SIZE_T Size,
    _In_ BOOLEAN CacheEnabled,
    _Out_ PVIRTIOSND_DMA_BUFFER Out);

VOID VirtIoSndFreeCommonBuffer(_In_ PVIRTIOSND_DMA_CONTEXT Ctx, _Inout_ PVIRTIOSND_DMA_BUFFER Buf);
