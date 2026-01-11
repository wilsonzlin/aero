/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <stdint.h>

#include "virtiosnd_queue.h"

/*
 * Minimal virtqueue stub for host tests.
 *
 * - Submit records the SG array (so tests can validate descriptor building).
 * - PopUsed drains a caller-injected used ring.
 * - A simple "inflight" capacity limit allows tests to simulate a full queue.
 */

typedef struct _VIRTIOSND_HOST_QUEUE_USED {
    void* Cookie;
    UINT32 UsedLen;
} VIRTIOSND_HOST_QUEUE_USED;

typedef struct _VIRTIOSND_HOST_QUEUE {
    VIRTIOSND_QUEUE Queue; /* Public-facing queue wrapper (Ops + Ctx). */

    USHORT Capacity;
    USHORT Inflight;

    /* Used ring (FIFO) */
    USHORT UsedHead;
    USHORT UsedTail;
    VIRTIOSND_HOST_QUEUE_USED Used[256];

    /* Last submission snapshot (for assertions). */
    void* LastCookie;
    USHORT LastSgCount;
    VIRTIOSND_SG LastSg[64];

    ULONG SubmitCalls;
    ULONG KickCalls;
    ULONG DisableInterruptCalls;
    ULONG EnableInterruptCalls;
} VIRTIOSND_HOST_QUEUE;

void VirtioSndHostQueueInit(_Out_ VIRTIOSND_HOST_QUEUE* Q, _In_ USHORT Capacity);
void VirtioSndHostQueueReset(_Inout_ VIRTIOSND_HOST_QUEUE* Q);

/*
 * Enqueue a used completion for PopUsed().
 *
 * The engine under test is responsible for storing status bytes in its own DMA
 * buffers before the completion is injected.
 */
void VirtioSndHostQueuePushUsed(_Inout_ VIRTIOSND_HOST_QUEUE* Q, _In_opt_ void* Cookie, _In_ UINT32 UsedLen);

