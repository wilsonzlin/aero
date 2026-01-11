/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_dma.h"

_Use_decl_annotations_
NTSTATUS VirtIoSndDmaInit(PDEVICE_OBJECT PhysicalDeviceObject, PVIRTIOSND_DMA_CONTEXT Ctx)
{
    UNREFERENCED_PARAMETER(PhysicalDeviceObject);
    if (Ctx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(Ctx, sizeof(*Ctx));
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID VirtIoSndDmaUninit(PVIRTIOSND_DMA_CONTEXT Ctx)
{
    if (Ctx == NULL) {
        return;
    }
    RtlZeroMemory(Ctx, sizeof(*Ctx));
}

_Use_decl_annotations_
NTSTATUS VirtIoSndAllocCommonBuffer(PVIRTIOSND_DMA_CONTEXT Ctx, SIZE_T Size, BOOLEAN CacheEnabled, PVIRTIOSND_DMA_BUFFER Out)
{
    void* p;

    if (Out != NULL) {
        RtlZeroMemory(Out, sizeof(*Out));
    }

    if (Ctx == NULL || Out == NULL || Size == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    p = calloc(1, Size);
    if (p == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Out->Va = p;
    Out->Size = Size;
    Out->DmaAddr = (UINT64)(uintptr_t)p;
    Out->IsCommonBuffer = TRUE;
    Out->CacheEnabled = CacheEnabled;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID VirtIoSndFreeCommonBuffer(PVIRTIOSND_DMA_CONTEXT Ctx, PVIRTIOSND_DMA_BUFFER Buf)
{
    VIRTIOSND_DMA_BUFFER tmp;
    BOOLEAN bufInAllocation;

    if (Buf == NULL || Buf->Va == NULL || Buf->Size == 0) {
        return;
    }

    /* Keep behavior aligned with the real driver implementation. */
    NT_ASSERT(Ctx != NULL);

    /*
     * The virtio-snd control engine stores its VIRTIOSND_DMA_BUFFER metadata inside
     * the allocation being freed. Avoid writing to *Buf after freeing Buf->Va.
     */
    tmp = *Buf;
    bufInAllocation = FALSE;
    {
        ULONGLONG start = (ULONGLONG)(UINT64)(uintptr_t)tmp.Va;
        ULONGLONG end = start + (ULONGLONG)tmp.Size;
        ULONGLONG addr = (ULONGLONG)(UINT64)(uintptr_t)Buf;
        if (end >= start && addr >= start && addr < end) {
            bufInAllocation = TRUE;
        }
    }

    free(tmp.Va);

    if (!bufInAllocation) {
        RtlZeroMemory(Buf, sizeof(*Buf));
    }
}
