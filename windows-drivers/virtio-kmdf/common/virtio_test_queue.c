#include "virtqueue_ring.h"

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
    VIRTQUEUE_RING_DMA ring;
    VIRTQUEUE_RING_DMA ringEvent;

    status = VirtioDmaCreate(Device, 64 * 1024, 32, TRUE, &dma);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioDmaAllocCommonBuffer(dma, 4096, 4096, FALSE, &buf);
    if (NT_SUCCESS(status)) {
        VIRTIO_DMA_TRACE("test queue buffer va=%p dma=0x%I64x len=%Iu\n", buf.Va, (unsigned long long)buf.Dma, buf.Length);
        VirtioDmaFreeCommonBuffer(&buf);
    }

    status = VirtqueueRingDmaAlloc(dma, NULL, 256, FALSE, &ring);
    if (NT_SUCCESS(status)) {
        VIRTIO_DMA_TRACE(
            "test queue ring desc=%p avail=%p used=%p descDma=0x%I64x availDma=0x%I64x usedDma=0x%I64x\n",
            ring.Desc,
            ring.Avail,
            ring.Used,
            (unsigned long long)ring.DescDma,
            (unsigned long long)ring.AvailDma,
            (unsigned long long)ring.UsedDma);
        VirtqueueRingDmaFree(&ring);
    }

    status = VirtqueueRingDmaAlloc(dma, NULL, 256, TRUE, &ringEvent);
    if (NT_SUCCESS(status)) {
        volatile UINT16* usedEvent = VirtqueueRingAvailUsedEvent(ringEvent.Avail, ringEvent.QueueSize);
        volatile UINT16* availEvent = VirtqueueRingUsedAvailEvent(ringEvent.Used, ringEvent.QueueSize);

        VIRTIO_DMA_TRACE(
            "test queue ring(EVENT_IDX) desc=%p avail=%p used=%p usedEvent=%p availEvent=%p\n",
            ringEvent.Desc,
            ringEvent.Avail,
            ringEvent.Used,
            usedEvent,
            availEvent);

        VirtqueueRingDmaFree(&ringEvent);
    }

    VirtioDmaDestroy(&dma);
    return status;
}
