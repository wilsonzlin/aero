/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <stdlib.h>
#include <string.h>

#include "virtiosnd_dma.h"

NTSTATUS VirtIoSndDmaInit(_In_ PDEVICE_OBJECT PhysicalDeviceObject, _Out_ PVIRTIOSND_DMA_CONTEXT Ctx)
{
    UNREFERENCED_PARAMETER(PhysicalDeviceObject);
    if (Ctx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(Ctx, sizeof(*Ctx));
    return STATUS_SUCCESS;
}

VOID VirtIoSndDmaUninit(_Inout_ PVIRTIOSND_DMA_CONTEXT Ctx)
{
    if (Ctx == NULL) {
        return;
    }
    RtlZeroMemory(Ctx, sizeof(*Ctx));
}

NTSTATUS VirtIoSndAllocCommonBuffer(
    _In_ PVIRTIOSND_DMA_CONTEXT Ctx,
    _In_ SIZE_T Size,
    _In_ BOOLEAN CacheEnabled,
    _Out_ PVIRTIOSND_DMA_BUFFER Out)
{
    void *mem;

    if (Out != NULL) {
        RtlZeroMemory(Out, sizeof(*Out));
    }

    if (Ctx == NULL || Out == NULL || Size == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    mem = malloc(Size);
    if (mem == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Out->Va = mem;
    Out->DmaAddr = (UINT64)(uintptr_t)mem;
    Out->Size = Size;
    Out->IsCommonBuffer = TRUE;
    Out->CacheEnabled = CacheEnabled;
    return STATUS_SUCCESS;
}

VOID VirtIoSndFreeCommonBuffer(_In_ PVIRTIOSND_DMA_CONTEXT Ctx, _Inout_ PVIRTIOSND_DMA_BUFFER Buf)
{
    VIRTIOSND_DMA_BUFFER tmp;
    BOOLEAN bufInAllocation;

    if (Buf == NULL || Buf->Va == NULL || Buf->Size == 0) {
        return;
    }

    /*
     * Keep behavior aligned with the real driver implementation: FreeCommonBuffer
     * requires a valid DMA context. This is important for tests that simulate
     * stop/remove races.
     */
    NT_ASSERT(Ctx != NULL);

    /*
     * The control protocol engine stores its VIRTIOSND_DMA_BUFFER metadata inside
     * the allocation being freed. Avoid writing to *Buf after freeing Buf->Va.
     */
    tmp = *Buf;
    bufInAllocation = FALSE;
    {
        uint64_t start = (uint64_t)(uintptr_t)tmp.Va;
        uint64_t end = start + (uint64_t)tmp.Size;
        uint64_t addr = (uint64_t)(uintptr_t)Buf;
        if (end >= start && addr >= start && addr < end) {
            bufInAllocation = TRUE;
        }
    }

    free(tmp.Va);

    if (!bufInAllocation) {
        RtlZeroMemory(Buf, sizeof(*Buf));
    }
}
