/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_tx.h"

static __forceinline ULONG VirtioSndTxHdrBytes(VOID) { return (ULONG)sizeof(VIRTIO_SND_TX_HDR); }
static __forceinline ULONG VirtioSndTxStatusBytes(VOID) { return (ULONG)sizeof(VIRTIO_SND_PCM_STATUS); }

ULONG VirtioSndTxFrameSizeBytes(VOID) { return 4u; }

static VOID VirtioSndTxFreeBuffers(_Inout_ VIRTIOSND_TX_ENGINE* Tx)
{
    ULONG i;

    if (Tx == NULL || Tx->Buffers == NULL) {
        return;
    }

    for (i = 0; i < Tx->BufferCount; ++i) {
        VirtIoSndFreeCommonBuffer(Tx->DmaCtx, &Tx->Buffers[i].Allocation);
    }

    ExFreePoolWithTag(Tx->Buffers, VIRTIOSND_POOL_TAG);
    Tx->Buffers = NULL;
    Tx->BufferCount = 0;
}

_Use_decl_annotations_
NTSTATUS
VirtioSndTxInit(
    VIRTIOSND_TX_ENGINE* Tx,
    PVIRTIOSND_DMA_CONTEXT DmaCtx,
    const VIRTIOSND_QUEUE* Queue,
    ULONG MaxPeriodBytes,
    ULONG BufferCount,
    BOOLEAN SuppressInterrupts)
{
    NTSTATUS status;
    ULONG i;
    ULONG outBytes;
    ULONG totalBytes;
    ULONG count;
    PUCHAR baseVa;
    VIRTIO_SND_TX_HDR* hdr;

    NT_ASSERT(Tx != NULL);
    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    if (Tx == NULL || DmaCtx == NULL || Queue == NULL || Queue->Ops == NULL || Queue->Ctx == NULL || Queue->Ops->Submit == NULL ||
        Queue->Ops->PopUsed == NULL || Queue->Ops->Kick == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (MaxPeriodBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    if ((MaxPeriodBytes % VirtioSndTxFrameSizeBytes()) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    count = BufferCount;
    if (count == 0) {
        count = 16u;
    }
    if (count > 64u) {
        count = 64u;
    }

    RtlZeroMemory(Tx, sizeof(*Tx));

    KeInitializeSpinLock(&Tx->Lock);
    InitializeListHead(&Tx->FreeList);
    InitializeListHead(&Tx->InflightList);

    Tx->Queue = Queue;
    Tx->DmaCtx = DmaCtx;

    Tx->MaxPeriodBytes = MaxPeriodBytes;
    Tx->NextSequence = 1;

    Tx->Buffers = (VIRTIOSND_TX_BUFFER*)ExAllocatePoolWithTag(NonPagedPool, sizeof(VIRTIOSND_TX_BUFFER) * (SIZE_T)count, VIRTIOSND_POOL_TAG);
    if (Tx->Buffers == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(Tx->Buffers, sizeof(VIRTIOSND_TX_BUFFER) * (SIZE_T)count);
    Tx->BufferCount = count;

    outBytes = VirtioSndTxHdrBytes() + MaxPeriodBytes;
    if (outBytes < MaxPeriodBytes) {
        VirtioSndTxFreeBuffers(Tx);
        return STATUS_INVALID_PARAMETER;
    }
    totalBytes = outBytes + VirtioSndTxStatusBytes();
    if (totalBytes < outBytes) {
        VirtioSndTxFreeBuffers(Tx);
        return STATUS_INVALID_PARAMETER;
    }

    for (i = 0; i < count; ++i) {
        status = VirtIoSndAllocCommonBuffer(Tx->DmaCtx, totalBytes, FALSE, &Tx->Buffers[i].Allocation);
        if (!NT_SUCCESS(status)) {
            goto Fail;
        }

        baseVa = (PUCHAR)Tx->Buffers[i].Allocation.Va;
        RtlZeroMemory(baseVa, totalBytes);

        Tx->Buffers[i].DataVa = baseVa;
        Tx->Buffers[i].DataDma = Tx->Buffers[i].Allocation.DmaAddr;

        Tx->Buffers[i].StatusVa = (VIRTIO_SND_PCM_STATUS*)(baseVa + outBytes);
        Tx->Buffers[i].StatusDma = Tx->Buffers[i].Allocation.DmaAddr + outBytes;

        Tx->Buffers[i].PcmBytes = 0;
        Tx->Buffers[i].Sequence = 0;
        Tx->Buffers[i].Inflight = FALSE;

        hdr = (VIRTIO_SND_TX_HDR*)Tx->Buffers[i].DataVa;
        hdr->stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
        hdr->reserved = 0;

        InsertTailList(&Tx->FreeList, &Tx->Buffers[i].Link);
        Tx->FreeCount++;
    }

    if (SuppressInterrupts) {
        VirtioSndQueueDisableInterrupts(Queue);
    }

    return STATUS_SUCCESS;

Fail:
    VirtioSndTxFreeBuffers(Tx);
    return status;
}

_Use_decl_annotations_
VOID
VirtioSndTxUninit(VIRTIOSND_TX_ENGINE* Tx)
{
    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    if (Tx == NULL) {
        return;
    }

    VirtioSndTxFreeBuffers(Tx);
    RtlZeroMemory(Tx, sizeof(*Tx));
}

static VOID VirtioSndTxReturnToFreeListLocked(_Inout_ VIRTIOSND_TX_ENGINE* Tx, _Inout_ VIRTIOSND_TX_BUFFER* Buffer)
{
    if (Buffer->Inflight) {
        RemoveEntryList(&Buffer->Link);
        Tx->InflightCount--;
        Buffer->Inflight = FALSE;
        InterlockedDecrement(&Tx->Stats.InFlight);
    }

    InsertTailList(&Tx->FreeList, &Buffer->Link);
    Tx->FreeCount++;
}

static VOID VirtioSndTxHandleUsedLocked(_Inout_ VIRTIOSND_TX_ENGINE* Tx, _Inout_ VIRTIOSND_TX_BUFFER* Buffer, _In_ UINT32 UsedLen)
{
    ULONG st;
    ULONG latency;

    /* Ensure device writes are visible before reading response bytes. */
    KeMemoryBarrier();

    st = VIRTIO_SND_S_BAD_MSG;
    latency = 0;
    if (UsedLen >= VirtioSndTxStatusBytes() && Buffer->StatusVa != NULL) {
        st = Buffer->StatusVa->status;
        latency = Buffer->StatusVa->latency_bytes;
    }

    Tx->LastVirtioStatus = st;
    Tx->LastLatencyBytes = latency;

    InterlockedIncrement(&Tx->Stats.Completed);

    switch (st) {
    case VIRTIO_SND_S_OK:
        InterlockedIncrement(&Tx->Stats.StatusOk);
        break;
    case VIRTIO_SND_S_BAD_MSG:
        InterlockedIncrement(&Tx->Stats.StatusBadMsg);
        Tx->FatalError = TRUE;
        break;
    case VIRTIO_SND_S_NOT_SUPP:
        InterlockedIncrement(&Tx->Stats.StatusNotSupp);
        Tx->FatalError = TRUE;
        break;
    case VIRTIO_SND_S_IO_ERR:
        InterlockedIncrement(&Tx->Stats.StatusIoErr);
        break;
    default:
        InterlockedIncrement(&Tx->Stats.StatusOther);
        break;
    }

    Buffer->PcmBytes = 0;
    VirtioSndTxReturnToFreeListLocked(Tx, Buffer);
}

_Use_decl_annotations_
NTSTATUS
VirtioSndTxSubmitPeriod(
    VIRTIOSND_TX_ENGINE* Tx,
    const VOID* Pcm1,
    ULONG Pcm1Bytes,
    const VOID* Pcm2,
    ULONG Pcm2Bytes,
    BOOLEAN AllowSilenceFill)
{
    ULONG totalPcmBytes;
    KIRQL oldIrql;
    LIST_ENTRY* entry;
    VIRTIOSND_TX_BUFFER* buf;
    PUCHAR dst;
    NTSTATUS status;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Tx == NULL || Tx->Queue == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (Tx->FatalError) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Pcm1Bytes != 0 && Pcm1 == NULL && !AllowSilenceFill) {
        return STATUS_INVALID_PARAMETER;
    }
    if (Pcm2Bytes != 0 && Pcm2 == NULL && !AllowSilenceFill) {
        return STATUS_INVALID_PARAMETER;
    }

    totalPcmBytes = Pcm1Bytes + Pcm2Bytes;
    if (totalPcmBytes < Pcm1Bytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    if (totalPcmBytes > Tx->MaxPeriodBytes || (totalPcmBytes % VirtioSndTxFrameSizeBytes()) != 0) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);

    if (Tx->FreeCount == 0 || IsListEmpty(&Tx->FreeList)) {
        InterlockedIncrement(&Tx->Stats.DroppedNoBuffers);
        KeReleaseSpinLock(&Tx->Lock, oldIrql);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    entry = RemoveHeadList(&Tx->FreeList);
    Tx->FreeCount--;
    buf = CONTAINING_RECORD(entry, VIRTIOSND_TX_BUFFER, Link);
    KeReleaseSpinLock(&Tx->Lock, oldIrql);

    buf->PcmBytes = totalPcmBytes;

    dst = (PUCHAR)buf->DataVa + VirtioSndTxHdrBytes();

    if (Pcm1Bytes != 0) {
        if (Pcm1 != NULL) {
            RtlCopyMemory(dst, Pcm1, Pcm1Bytes);
        } else {
            RtlZeroMemory(dst, Pcm1Bytes);
        }
    }
    if (Pcm2Bytes != 0) {
        if (Pcm2 != NULL) {
            RtlCopyMemory(dst + Pcm1Bytes, Pcm2, Pcm2Bytes);
        } else {
            RtlZeroMemory(dst + Pcm1Bytes, Pcm2Bytes);
        }
    }

    RtlZeroMemory(buf->StatusVa, sizeof(*buf->StatusVa));

    buf->Sg[0].addr = buf->DataDma;
    buf->Sg[0].len = (UINT32)(VirtioSndTxHdrBytes() + totalPcmBytes);
    buf->Sg[0].write = FALSE;

    buf->Sg[1].addr = buf->StatusDma;
    buf->Sg[1].len = (UINT32)VirtioSndTxStatusBytes();
    buf->Sg[1].write = TRUE;

    /* Ensure header/data/status writes are visible before publishing descriptors. */
    KeMemoryBarrier();

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);

    buf->Sequence = Tx->NextSequence++;
    status = VirtioSndQueueSubmit(Tx->Queue, buf->Sg, 2, buf);

    if (!NT_SUCCESS(status)) {
        InsertTailList(&Tx->FreeList, &buf->Link);
        Tx->FreeCount++;
        InterlockedIncrement(&Tx->Stats.SubmitErrors);
        KeReleaseSpinLock(&Tx->Lock, oldIrql);
        return (status == STATUS_INSUFFICIENT_RESOURCES) ? STATUS_INSUFFICIENT_RESOURCES : status;
    }

    InsertTailList(&Tx->InflightList, &buf->Link);
    Tx->InflightCount++;
    buf->Inflight = TRUE;

    InterlockedIncrement(&Tx->Stats.Submitted);
    InterlockedIncrement(&Tx->Stats.InFlight);

    KeReleaseSpinLock(&Tx->Lock, oldIrql);

    VirtioSndQueueKick(Tx->Queue);

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS
VirtioSndTxSubmitSg(VIRTIOSND_TX_ENGINE* Tx, const VIRTIOSND_TX_SEGMENT* Segments, ULONG SegmentCount)
{
    ULONGLONG totalBytes;
    ULONG i;
    KIRQL oldIrql;
    LIST_ENTRY* entry;
    VIRTIOSND_TX_BUFFER* buf;
    NTSTATUS status;
    USHORT sgCount;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Tx == NULL || Segments == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (Tx->Queue == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (Tx->FatalError) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (SegmentCount == 0 || SegmentCount > VIRTIOSND_TX_MAX_SEGMENTS) {
        return STATUS_INVALID_PARAMETER;
    }

    totalBytes = 0;
    for (i = 0; i < SegmentCount; ++i) {
        if (Segments[i].Length == 0) {
            return STATUS_INVALID_PARAMETER;
        }
        totalBytes += (ULONGLONG)Segments[i].Length;
        if (totalBytes > 0xFFFFFFFFull) {
            return STATUS_INVALID_BUFFER_SIZE;
        }
    }

    if (totalBytes > (ULONGLONG)Tx->MaxPeriodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    if ((totalBytes % (ULONGLONG)VirtioSndTxFrameSizeBytes()) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);

    if (Tx->FreeCount == 0 || IsListEmpty(&Tx->FreeList)) {
        InterlockedIncrement(&Tx->Stats.DroppedNoBuffers);
        KeReleaseSpinLock(&Tx->Lock, oldIrql);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    entry = RemoveHeadList(&Tx->FreeList);
    Tx->FreeCount--;
    buf = CONTAINING_RECORD(entry, VIRTIOSND_TX_BUFFER, Link);
    KeReleaseSpinLock(&Tx->Lock, oldIrql);

    buf->PcmBytes = (ULONG)totalBytes;
    RtlZeroMemory(buf->StatusVa, sizeof(*buf->StatusVa));

    /* SG: header (8 bytes) */
    buf->Sg[0].addr = buf->DataDma;
    buf->Sg[0].len = (UINT32)VirtioSndTxHdrBytes();
    buf->Sg[0].write = FALSE;

    /* SG: PCM segments */
    for (i = 0; i < SegmentCount; ++i) {
        buf->Sg[1u + i].addr = (UINT64)Segments[i].Address.QuadPart;
        buf->Sg[1u + i].len = (UINT32)Segments[i].Length;
        buf->Sg[1u + i].write = FALSE;
    }

    /* SG: status (8 bytes) */
    buf->Sg[1u + SegmentCount].addr = buf->StatusDma;
    buf->Sg[1u + SegmentCount].len = (UINT32)VirtioSndTxStatusBytes();
    buf->Sg[1u + SegmentCount].write = TRUE;

    sgCount = (USHORT)(SegmentCount + 2u);

    KeMemoryBarrier();

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);

    buf->Sequence = Tx->NextSequence++;
    status = VirtioSndQueueSubmit(Tx->Queue, buf->Sg, sgCount, buf);
    if (!NT_SUCCESS(status)) {
        InsertTailList(&Tx->FreeList, &buf->Link);
        Tx->FreeCount++;
        InterlockedIncrement(&Tx->Stats.SubmitErrors);
        KeReleaseSpinLock(&Tx->Lock, oldIrql);
        return (status == STATUS_INSUFFICIENT_RESOURCES) ? STATUS_INSUFFICIENT_RESOURCES : status;
    }

    InsertTailList(&Tx->InflightList, &buf->Link);
    Tx->InflightCount++;
    buf->Inflight = TRUE;

    InterlockedIncrement(&Tx->Stats.Submitted);
    InterlockedIncrement(&Tx->Stats.InFlight);

    KeReleaseSpinLock(&Tx->Lock, oldIrql);

    VirtioSndQueueKick(Tx->Queue);

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
ULONG
VirtioSndTxDrainCompletions(VIRTIOSND_TX_ENGINE* Tx)
{
    KIRQL oldIrql;
    VOID* ctx;
    UINT32 usedLen;
    VIRTIOSND_TX_BUFFER* buf;
    ULONG drained;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Tx == NULL || Tx->Queue == NULL) {
        return 0;
    }

    drained = 0;

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);

    while (VirtioSndQueuePopUsed(Tx->Queue, &ctx, &usedLen)) {
        buf = (VIRTIOSND_TX_BUFFER*)ctx;
        if (buf == NULL) {
            continue;
        }
        VirtioSndTxHandleUsedLocked(Tx, buf, usedLen);
        drained++;
    }

    KeReleaseSpinLock(&Tx->Lock, oldIrql);
    return drained;
}

_Use_decl_annotations_
VOID
VirtioSndTxProcessCompletions(VIRTIOSND_TX_ENGINE* Tx)
{
    (VOID)VirtioSndTxDrainCompletions(Tx);
}

_Use_decl_annotations_
VOID
VirtioSndTxOnUsed(VIRTIOSND_TX_ENGINE* Tx, void* Cookie, UINT32 UsedLen)
{
    KIRQL oldIrql;
    VIRTIOSND_TX_BUFFER* buf;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Tx == NULL || Cookie == NULL) {
        return;
    }
    if (Tx->Queue == NULL) {
        return;
    }

    buf = (VIRTIOSND_TX_BUFFER*)Cookie;

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);
    VirtioSndTxHandleUsedLocked(Tx, buf, UsedLen);
    KeReleaseSpinLock(&Tx->Lock, oldIrql);
}
