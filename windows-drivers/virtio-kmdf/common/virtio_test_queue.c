#include "virtio_dma.h"

//
// Minimal usage example for the virtio_dma module.
//
// This is intended as a drop-in snippet for a driver's EvtDevicePrepareHardware
// (or equivalent) while bringing up virtqueue code.
//
_Must_inspect_result_
NTSTATUS VirtioTestQueueAllocAndLog(_In_ WDFDEVICE Device)
{
    NTSTATUS status;
    VIRTIO_DMA_CONTEXT* dma = NULL;
    VIRTIO_COMMON_BUFFER buf;

    status = VirtioDmaCreate(Device, 64 * 1024, 32, TRUE, &dma);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioDmaAllocCommonBuffer(dma, 4096, 4096, FALSE, &buf);
    if (NT_SUCCESS(status)) {
        VIRTIO_DMA_TRACE("test queue buffer va=%p dma=0x%I64x len=%Iu\n", buf.Va, (unsigned long long)buf.Dma, buf.Length);
        VirtioDmaFreeCommonBuffer(&buf);
    }

    VirtioDmaDestroy(&dma);
    return status;
}

