/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "virtiosnd_queue.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Minimal fake VIRTIOSND_QUEUE implementation for host unit tests.
 *
 * - Captures the last submitted SG list + cookie.
 * - Optionally auto-completes submissions on Kick() by pushing an entry into the
 *   used queue and writing VIRTIO_SND_S_OK into the last device-writable
 *   descriptor (status/resp buffer).
 */

#define VIRTIO_TEST_QUEUE_MAX_SG 32
#define VIRTIO_TEST_QUEUE_MAX_PENDING 64

typedef struct _VIRTIO_TEST_QUEUE_CAPTURE {
    VIRTIOSND_SG sg[VIRTIO_TEST_QUEUE_MAX_SG];
    USHORT sg_count;
    void *cookie;

    /*
     * Copy of sg[0] bytes at submission time.
     *
     * This is important for controlq requests, whose request DMA buffer is
     * freed before the caller can inspect it.
     */
    uint8_t *out0_copy;
    size_t out0_copy_len;
} VIRTIO_TEST_QUEUE_CAPTURE;

typedef struct _VIRTIO_TEST_QUEUE {
    VIRTIOSND_QUEUE queue;
    VIRTIOSND_QUEUE_OPS ops;

    /* Captured most recent Submit() call. */
    VIRTIO_TEST_QUEUE_CAPTURE last;
    uint32_t submit_count;
    uint32_t kick_count;

    struct {
        VIRTIOSND_SG sg[VIRTIO_TEST_QUEUE_MAX_SG];
        USHORT sg_count;
        void *cookie;
    } pending[VIRTIO_TEST_QUEUE_MAX_PENDING];
    size_t pending_count;

    struct {
        void *cookie;
        UINT32 used_len;
    } used[VIRTIO_TEST_QUEUE_MAX_PENDING];
    size_t used_head;
    size_t used_tail;
    size_t used_count;

    BOOLEAN auto_complete;
} VIRTIO_TEST_QUEUE;

void virtio_test_queue_init(VIRTIO_TEST_QUEUE *q, BOOLEAN auto_complete);
void virtio_test_queue_reset(VIRTIO_TEST_QUEUE *q);
void virtio_test_queue_destroy(VIRTIO_TEST_QUEUE *q);

const VIRTIO_TEST_QUEUE_CAPTURE *virtio_test_queue_last(const VIRTIO_TEST_QUEUE *q);

#ifdef __cplusplus
} /* extern "C" */
#endif

