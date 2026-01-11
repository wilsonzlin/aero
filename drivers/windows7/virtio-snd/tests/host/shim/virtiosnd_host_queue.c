/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_host_queue.h"

static NTSTATUS HostQueueSubmit(_In_ void* ctx, _In_reads_(sg_count) const VIRTIOSND_SG* sg, _In_ USHORT sg_count, _In_opt_ void* cookie)
{
    VIRTIOSND_HOST_QUEUE* q;
    USHORT i;

    q = (VIRTIOSND_HOST_QUEUE*)ctx;
    if (q == NULL || sg == NULL || sg_count == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    q->SubmitCalls++;

    if (q->Inflight >= q->Capacity) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    q->Inflight++;

    q->LastCookie = cookie;
    q->LastSgCount = sg_count;
    if (sg_count > RTL_NUMBER_OF(q->LastSg)) {
        sg_count = (USHORT)RTL_NUMBER_OF(q->LastSg);
    }
    for (i = 0; i < sg_count; i++) {
        q->LastSg[i] = sg[i];
    }

    return STATUS_SUCCESS;
}

static BOOLEAN HostQueuePopUsed(_In_ void* ctx, _Out_ void** cookie_out, _Out_ UINT32* used_len_out)
{
    VIRTIOSND_HOST_QUEUE* q;
    USHORT head;
    VIRTIOSND_HOST_QUEUE_USED* ent;

    q = (VIRTIOSND_HOST_QUEUE*)ctx;
    if (q == NULL || cookie_out == NULL || used_len_out == NULL) {
        return FALSE;
    }

    if (q->UsedHead == q->UsedTail) {
        return FALSE;
    }

    head = q->UsedHead;
    ent = &q->Used[head];
    q->UsedHead = (USHORT)((head + 1u) % (USHORT)RTL_NUMBER_OF(q->Used));

    *cookie_out = ent->Cookie;
    *used_len_out = ent->UsedLen;

    if (q->Inflight != 0) {
        q->Inflight--;
    }

    return TRUE;
}

static VOID HostQueueKick(_In_ void* ctx)
{
    VIRTIOSND_HOST_QUEUE* q;

    q = (VIRTIOSND_HOST_QUEUE*)ctx;
    if (q == NULL) {
        return;
    }
    q->KickCalls++;
}

static VOID HostQueueDisableInterrupts(_In_ void* ctx)
{
    VIRTIOSND_HOST_QUEUE* q;

    q = (VIRTIOSND_HOST_QUEUE*)ctx;
    if (q == NULL) {
        return;
    }
    q->DisableInterruptCalls++;
}

static BOOLEAN HostQueueEnableInterrupts(_In_ void* ctx)
{
    VIRTIOSND_HOST_QUEUE* q;

    q = (VIRTIOSND_HOST_QUEUE*)ctx;
    if (q == NULL) {
        return FALSE;
    }
    q->EnableInterruptCalls++;
    return TRUE;
}

static const VIRTIOSND_QUEUE_OPS g_hostQueueOps = {
    HostQueueSubmit,
    HostQueuePopUsed,
    HostQueueKick,
    NULL,
    HostQueueDisableInterrupts,
    HostQueueEnableInterrupts,
};

void VirtioSndHostQueueInit(_Out_ VIRTIOSND_HOST_QUEUE* Q, _In_ USHORT Capacity)
{
    NT_ASSERT(Q != NULL);

    RtlZeroMemory(Q, sizeof(*Q));
    Q->Capacity = Capacity;
    if (Q->Capacity == 0) {
        Q->Capacity = 1;
    }

    Q->Queue.Ops = &g_hostQueueOps;
    Q->Queue.Ctx = Q;
}

void VirtioSndHostQueueReset(_Inout_ VIRTIOSND_HOST_QUEUE* Q)
{
    USHORT cap;

    if (Q == NULL) {
        return;
    }

    cap = Q->Capacity;
    RtlZeroMemory(Q, sizeof(*Q));
    Q->Capacity = cap != 0 ? cap : 1;

    Q->Queue.Ops = &g_hostQueueOps;
    Q->Queue.Ctx = Q;
}

void VirtioSndHostQueuePushUsed(_Inout_ VIRTIOSND_HOST_QUEUE* Q, _In_opt_ void* Cookie, _In_ UINT32 UsedLen)
{
    USHORT nextTail;

    NT_ASSERT(Q != NULL);

    nextTail = (USHORT)((Q->UsedTail + 1u) % (USHORT)RTL_NUMBER_OF(Q->Used));
    NT_ASSERT(nextTail != Q->UsedHead); /* queue full */

    Q->Used[Q->UsedTail].Cookie = Cookie;
    Q->Used[Q->UsedTail].UsedLen = UsedLen;
    Q->UsedTail = nextTail;
}
