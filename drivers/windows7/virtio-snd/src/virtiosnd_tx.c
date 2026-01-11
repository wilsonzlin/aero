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

    if (Tx->Buffers == NULL) {
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
    ULONG BufferCount)
{
    NTSTATUS status;
    ULONG i;
    ULONG outBytes;
    ULONG totalBytes;
    PUCHAR baseVa;
    VIRTIO_SND_TX_HDR* hdr;

    NT_ASSERT(Tx != NULL);
    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    if (Tx == NULL || Queue == NULL || Queue->Ops == NULL || Queue->Ctx == NULL || Queue->Ops->Submit == NULL || Queue->Ops->PopUsed == NULL ||
        Queue->Ops->Kick == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (DmaCtx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (MaxPeriodBytes == 0 || BufferCount == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Tx, sizeof(*Tx));

    KeInitializeSpinLock(&Tx->Lock);
    InitializeListHead(&Tx->FreeList);
    InitializeListHead(&Tx->InflightList);

    Tx->Queue = Queue;
    Tx->DmaCtx = DmaCtx;

    Tx->MaxPeriodBytes = MaxPeriodBytes;
    Tx->NextSequence = 1;

    Tx->Buffers =
        (VIRTIOSND_TX_BUFFER*)ExAllocatePoolWithTag(NonPagedPool, sizeof(VIRTIOSND_TX_BUFFER) * BufferCount, VIRTIOSND_POOL_TAG);
    if (Tx->Buffers == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(Tx->Buffers, sizeof(VIRTIOSND_TX_BUFFER) * BufferCount);
    Tx->BufferCount = BufferCount;

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

    for (i = 0; i < BufferCount; ++i) {
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
    }

    InsertTailList(&Tx->FreeList, &Buffer->Link);
    Tx->FreeCount++;
}

static VOID VirtioSndTxHandleUsedLocked(_Inout_ VIRTIOSND_TX_ENGINE* Tx, _Inout_ VIRTIOSND_TX_BUFFER* Buffer)
{
    ULONG st;
    ULONG latency;

    st = Buffer->StatusVa->status;
    latency = Buffer->StatusVa->latency_bytes;

    Tx->LastVirtioStatus = st;
    Tx->LastLatencyBytes = latency;

    if (st == VIRTIO_SND_S_OK) {
        Tx->CompletedOk++;
    } else if (st == VIRTIO_SND_S_IO_ERR) {
        Tx->CompletedIoErr++;
    } else {
        Tx->CompletedBadMsgOrNotSupp++;
        if (st == VIRTIO_SND_S_BAD_MSG || st == VIRTIO_SND_S_NOT_SUPP) {
            Tx->FatalError = TRUE;
        }
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
    VIRTIOSND_SG sg[2];

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Tx == NULL || Tx->Queue == NULL) {
        return STATUS_INVALID_PARAMETER;
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
        Tx->DroppedDueToNoBuffers++;
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

    sg[0].addr = buf->DataDma;
    sg[0].len = (UINT32)(VirtioSndTxHdrBytes() + totalPcmBytes);
    sg[0].write = FALSE;

    sg[1].addr = buf->StatusDma;
    sg[1].len = (UINT32)VirtioSndTxStatusBytes();
    sg[1].write = TRUE;

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);

    buf->Sequence = Tx->NextSequence++;
    status = VirtioSndQueueSubmit(Tx->Queue, sg, 2, buf);

    if (!NT_SUCCESS(status)) {
        InsertTailList(&Tx->FreeList, &buf->Link);
        Tx->FreeCount++;
        KeReleaseSpinLock(&Tx->Lock, oldIrql);
        return status;
    }

    InsertTailList(&Tx->InflightList, &buf->Link);
    Tx->InflightCount++;
    buf->Inflight = TRUE;

    Tx->SubmittedPeriods++;
    KeReleaseSpinLock(&Tx->Lock, oldIrql);

    VirtioSndQueueKick(Tx->Queue);

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
VirtioSndTxProcessCompletions(VIRTIOSND_TX_ENGINE* Tx)
{
    KIRQL oldIrql;
    VOID* ctx;
    UINT32 usedLen;
    VIRTIOSND_TX_BUFFER* buf;

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Tx == NULL || Tx->Queue == NULL) {
        return;
    }

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);

    while (VirtioSndQueuePopUsed(Tx->Queue, &ctx, &usedLen)) {
        UNREFERENCED_PARAMETER(usedLen);

        buf = (VIRTIOSND_TX_BUFFER*)ctx;
        if (buf == NULL) {
            continue;
        }
        VirtioSndTxHandleUsedLocked(Tx, buf);
    }

    KeReleaseSpinLock(&Tx->Lock, oldIrql);
}

_Use_decl_annotations_
VOID
VirtioSndTxOnUsed(VIRTIOSND_TX_ENGINE* Tx, void* Cookie, UINT32 UsedLen)
{
    KIRQL oldIrql;
    VIRTIOSND_TX_BUFFER* buf;

    UNREFERENCED_PARAMETER(UsedLen);

    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

    if (Tx == NULL || Cookie == NULL) {
        return;
    }
    if (Tx->Queue == NULL) {
        return;
    }

    buf = (VIRTIOSND_TX_BUFFER*)Cookie;

    KeAcquireSpinLock(&Tx->Lock, &oldIrql);
    VirtioSndTxHandleUsedLocked(Tx, buf);
    KeReleaseSpinLock(&Tx->Lock, oldIrql);
}
