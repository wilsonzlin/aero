/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

/*
 * virtio-snd queue abstraction.
 *
 * This is intentionally small and keeps higher-level virtio-snd protocol/control
 * code transport-agnostic. Concrete implementations (e.g. split virtqueues) are
 * expected to provide an ops table + context pointer.
 *
 * Contract v1 queue indices/sizes:
 *  - 0: controlq (64)
 *  - 1: eventq   (64)
 *  - 2: txq     (256)
 *  - 3: rxq      (64) exists for PCM capture. The current driver wires it up
 *    for transport bring-up but does not submit capture buffers yet.
 */

#define VIRTIOSND_QUEUE_INDEX_CONTROLQ ((USHORT)0u)
#define VIRTIOSND_QUEUE_INDEX_EVENTQ ((USHORT)1u)
#define VIRTIOSND_QUEUE_INDEX_TXQ ((USHORT)2u)
#define VIRTIOSND_QUEUE_INDEX_RXQ ((USHORT)3u) /* Capture queue (buffers not submitted yet). */

#define VIRTIOSND_QUEUE_SIZE_CONTROLQ ((USHORT)64u)
#define VIRTIOSND_QUEUE_SIZE_EVENTQ ((USHORT)64u)
#define VIRTIOSND_QUEUE_SIZE_TXQ ((USHORT)256u)
#define VIRTIOSND_QUEUE_SIZE_RXQ ((USHORT)64u) /* Capture queue (buffers not submitted yet). */

typedef struct _VIRTIOSND_SG {
    UINT64 addr;
    UINT32 len;
    BOOLEAN write; /* device_writes (VRING_DESC_F_WRITE) */
} VIRTIOSND_SG, *PVIRTIOSND_SG;

typedef struct _VIRTIOSND_QUEUE_OPS {
    _Must_inspect_result_ NTSTATUS (*Submit)(
        _In_ void *ctx,
        _In_reads_(sg_count) const VIRTIOSND_SG *sg,
        _In_ USHORT sg_count,
        _In_opt_ void *cookie);

    _Must_inspect_result_ BOOLEAN (*PopUsed)(
        _In_ void *ctx,
        _Out_ void **cookie_out,
        _Out_ UINT32 *used_len_out);

    VOID (*Kick)(_In_ void *ctx);
} VIRTIOSND_QUEUE_OPS, *PVIRTIOSND_QUEUE_OPS;

typedef struct _VIRTIOSND_QUEUE {
    const VIRTIOSND_QUEUE_OPS *Ops;
    void *Ctx;
} VIRTIOSND_QUEUE, *PVIRTIOSND_QUEUE;

static __inline NTSTATUS
VirtioSndQueueSubmit(
    _In_ const VIRTIOSND_QUEUE *Queue,
    _In_reads_(SgCount) const VIRTIOSND_SG *Sg,
    _In_ USHORT SgCount,
    _In_opt_ void *Cookie)
{
    return Queue->Ops->Submit(Queue->Ctx, Sg, SgCount, Cookie);
}

static __inline VOID
VirtioSndQueueKick(_In_ const VIRTIOSND_QUEUE *Queue)
{
    Queue->Ops->Kick(Queue->Ctx);
}

static __inline BOOLEAN
VirtioSndQueuePopUsed(_In_ const VIRTIOSND_QUEUE *Queue, _Out_ void **CookieOut, _Out_ UINT32 *UsedLenOut)
{
    return Queue->Ops->PopUsed(Queue->Ctx, CookieOut, UsedLenOut);
}
