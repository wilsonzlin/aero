/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <stdlib.h>
#include <string.h>

#include "test_queue.h"
#include "virtio_snd_proto.h"

static void virtio_test_queue_free_last_capture(VIRTIO_TEST_QUEUE *q)
{
    if (q->last.out0_copy != NULL) {
        free(q->last.out0_copy);
        q->last.out0_copy = NULL;
        q->last.out0_copy_len = 0;
    }
}

static NTSTATUS virtio_test_queue_submit(void *ctx, const VIRTIOSND_SG *sg, USHORT sg_count, void *cookie)
{
    VIRTIO_TEST_QUEUE *q;
    USHORT i;

    q = (VIRTIO_TEST_QUEUE *)ctx;
    if (q == NULL || sg == NULL || sg_count == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    if (sg_count > VIRTIO_TEST_QUEUE_MAX_SG) {
        return STATUS_INVALID_PARAMETER;
    }
    if (q->pending_count >= VIRTIO_TEST_QUEUE_MAX_PENDING) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    virtio_test_queue_free_last_capture(q);

    q->last.sg_count = sg_count;
    q->last.cookie = cookie;
    for (i = 0; i < sg_count; ++i) {
        q->last.sg[i] = sg[i];
    }

    /* Snapshot sg[0] contents for later inspection (e.g. controlq requests). */
    q->last.out0_copy_len = (size_t)sg[0].len;
    q->last.out0_copy = NULL;
    if (q->last.out0_copy_len != 0 && sg[0].addr != 0) {
        q->last.out0_copy = (uint8_t *)malloc(q->last.out0_copy_len);
        if (q->last.out0_copy == NULL) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        memcpy(q->last.out0_copy, (const void *)(uintptr_t)sg[0].addr, q->last.out0_copy_len);
    }

    q->pending[q->pending_count].sg_count = sg_count;
    q->pending[q->pending_count].cookie = cookie;
    for (i = 0; i < sg_count; ++i) {
        q->pending[q->pending_count].sg[i] = sg[i];
    }
    q->pending_count++;

    q->submit_count++;
    return STATUS_SUCCESS;
}

static BOOLEAN virtio_test_queue_pop_used(void *ctx, void **cookie_out, UINT32 *used_len_out)
{
    VIRTIO_TEST_QUEUE *q = (VIRTIO_TEST_QUEUE *)ctx;
    if (q == NULL || cookie_out == NULL || used_len_out == NULL) {
        return FALSE;
    }

    if (q->used_count == 0) {
        return FALSE;
    }

    *cookie_out = q->used[q->used_head].cookie;
    *used_len_out = q->used[q->used_head].used_len;

    q->used_head = (q->used_head + 1u) % VIRTIO_TEST_QUEUE_MAX_PENDING;
    q->used_count--;
    return TRUE;
}

static void virtio_test_queue_complete_one(VIRTIO_TEST_QUEUE *q, size_t pending_index)
{
    UINT32 used_len;
    USHORT i;
    const VIRTIOSND_SG *sg;
    USHORT sg_count;

    sg = q->pending[pending_index].sg;
    sg_count = q->pending[pending_index].sg_count;

    /* Compute used length as sum of device-writable descriptors. */
    used_len = 0;
    for (i = 0; i < sg_count; ++i) {
        if (sg[i].write) {
            used_len += sg[i].len;
        }
    }

    /* Write a successful status to the final writable descriptor, if any. */
    for (i = sg_count; i > 0; --i) {
        const VIRTIOSND_SG *ent = &sg[i - 1u];
        if (ent->write && ent->addr != 0 && ent->len >= sizeof(uint32_t)) {
            if (ent->len >= sizeof(VIRTIO_SND_PCM_STATUS)) {
                VIRTIO_SND_PCM_STATUS *st = (VIRTIO_SND_PCM_STATUS *)(uintptr_t)ent->addr;
                st->status = VIRTIO_SND_S_OK;
                st->latency_bytes = 0;
            } else {
                uint32_t *status = (uint32_t *)(uintptr_t)ent->addr;
                *status = VIRTIO_SND_S_OK;
            }
            break;
        }
    }

    if (q->used_count < VIRTIO_TEST_QUEUE_MAX_PENDING) {
        q->used[q->used_tail].cookie = q->pending[pending_index].cookie;
        q->used[q->used_tail].used_len = used_len;
        q->used_tail = (q->used_tail + 1u) % VIRTIO_TEST_QUEUE_MAX_PENDING;
        q->used_count++;
    }
}

static void virtio_test_queue_kick(void *ctx)
{
    VIRTIO_TEST_QUEUE *q;
    size_t i;

    q = (VIRTIO_TEST_QUEUE *)ctx;
    if (q == NULL) {
        return;
    }

    q->kick_count++;

    if (!q->auto_complete) {
        return;
    }

    for (i = 0; i < q->pending_count; ++i) {
        virtio_test_queue_complete_one(q, i);
    }
    q->pending_count = 0;
}

void virtio_test_queue_init(VIRTIO_TEST_QUEUE *q, BOOLEAN auto_complete)
{
    memset(q, 0, sizeof(*q));
    q->auto_complete = auto_complete;

    q->ops.Submit = virtio_test_queue_submit;
    q->ops.PopUsed = virtio_test_queue_pop_used;
    q->ops.Kick = virtio_test_queue_kick;
    q->ops.DisableInterrupts = NULL;
    q->ops.EnableInterrupts = NULL;

    q->queue.Ops = &q->ops;
    q->queue.Ctx = q;
}

void virtio_test_queue_reset(VIRTIO_TEST_QUEUE *q)
{
    if (q == NULL) {
        return;
    }

    virtio_test_queue_free_last_capture(q);

    memset(&q->last, 0, sizeof(q->last));
    q->submit_count = 0;
    q->kick_count = 0;
    q->pending_count = 0;
    q->used_head = 0;
    q->used_tail = 0;
    q->used_count = 0;
}

void virtio_test_queue_destroy(VIRTIO_TEST_QUEUE *q)
{
    if (q == NULL) {
        return;
    }
    virtio_test_queue_free_last_capture(q);
}

const VIRTIO_TEST_QUEUE_CAPTURE *virtio_test_queue_last(const VIRTIO_TEST_QUEUE *q) { return &q->last; }

