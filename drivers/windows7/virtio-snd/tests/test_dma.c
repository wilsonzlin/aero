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

    UNREFERENCED_PARAMETER(Ctx);

    if (Out == NULL || Size == 0) {
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
    UNREFERENCED_PARAMETER(Ctx);
    if (Buf == NULL) {
        return;
    }
    if (Buf->Va != NULL) {
        free(Buf->Va);
    }
    RtlZeroMemory(Buf, sizeof(*Buf));
}

