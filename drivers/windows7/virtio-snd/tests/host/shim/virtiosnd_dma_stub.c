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

    UNREFERENCED_PARAMETER(Ctx);

    if (Out == NULL || Size == 0) {
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
    UNREFERENCED_PARAMETER(Ctx);

    if (Buf == NULL) {
        return;
    }

    if (Buf->Va != NULL) {
        free(Buf->Va);
    }

    RtlZeroMemory(Buf, sizeof(*Buf));
}

