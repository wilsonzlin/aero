/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_legacy.h"
#include "virtqueue_split_legacy.h"

#include "fake_pci_device.h"
#include "test_os.h"

/*
 * This test harness relies heavily on assert() for both validation and to
 * execute side-effectful setup calls. CMake Release builds define NDEBUG, which
 * would compile out all assert() expressions and skip those setup calls,
 * causing undefined behaviour and crashes.
 *
 * Override assert() so it remains active in all build configurations.
 */
#undef assert
#define assert(expr)                                                                                                   \
    do {                                                                                                               \
        if (!(expr)) {                                                                                                 \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                \
            abort();                                                                                                   \
        }                                                                                                              \
    } while (0)

typedef struct vring_device_sim {
    virtqueue_split_t *vq;
    uint16_t last_avail_idx;
    uint16_t notify_batch;
} vring_device_sim_t;

static void sim_process(vring_device_sim_t *sim);

static uint32_t rng_state = 0x12345678u;

static uint32_t rng_next(void)
{
    /* xorshift32 */
    uint32_t x = rng_state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    rng_state = x;
    return x;
}

static size_t align_up_size(size_t v, size_t align)
{
    return (v + (align - 1u)) & ~(align - 1u);
}

static void test_ring_size_event_idx(void)
{
    /*
     * Validate virtqueue_split_ring_size() math with and without EVENT_IDX.
     *
     * Using queue_align=4 ensures the EVENT_IDX fields affect the used ring
     * offset and overall size (unlike 4096 where everything rounds up).
     */
    const uint16_t qsz = 8;
    const uint32_t align = 4;

    size_t got_no_event;
    size_t got_event;
    size_t desc_size;
    size_t avail_no;
    size_t avail_event;
    size_t used_no;
    size_t used_event;
    size_t used_off_no;
    size_t used_off_event;
    size_t exp_no;
    size_t exp_event;

    got_no_event = virtqueue_split_ring_size(qsz, align, VIRTIO_FALSE);
    got_event = virtqueue_split_ring_size(qsz, align, VIRTIO_TRUE);

    desc_size = sizeof(vring_desc_t) * (size_t)qsz;
    avail_no = sizeof(uint16_t) * 2u + sizeof(uint16_t) * (size_t)qsz;
    avail_event = avail_no + sizeof(uint16_t);
    used_no = sizeof(uint16_t) * 2u + sizeof(vring_used_elem_t) * (size_t)qsz;
    used_event = used_no + sizeof(uint16_t);

    used_off_no = align_up_size(desc_size + avail_no, align);
    used_off_event = align_up_size(desc_size + avail_event, align);
    exp_no = align_up_size(used_off_no + used_no, align);
    exp_event = align_up_size(used_off_event + used_event, align);

    assert(got_no_event == exp_no);
    assert(got_event == exp_event);
    assert(got_event >= got_no_event);

    /* With legacy 4K alignment, the sizes round up to page multiples. */
    assert(virtqueue_split_ring_size(qsz, 4096, VIRTIO_FALSE) == 8192);
    assert(virtqueue_split_ring_size(qsz, 4096, VIRTIO_TRUE) == 8192);
}

static void test_interrupt_suppression_helpers(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg;
    uint16_t head;
    void *cookie_out;
    uint32_t used_len;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    /*
     * Legacy interrupt suppression (no EVENT_IDX): toggles VRING_AVAIL_F_NO_INTERRUPT.
     */
    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);
    assert((vq.avail->flags & VRING_AVAIL_F_NO_INTERRUPT) == 0);

    virtqueue_split_disable_interrupts(&vq);
    assert((vq.avail->flags & VRING_AVAIL_F_NO_INTERRUPT) != 0);
    assert(vq.used_event == NULL);

    /* No completions -> enable returns FALSE. */
    assert(virtqueue_split_enable_interrupts(&vq) == VIRTIO_FALSE);
    assert((vq.avail->flags & VRING_AVAIL_F_NO_INTERRUPT) == 0);

    /* Pre-existing completion -> enable returns TRUE (needs drain). */
    vq.used->idx = 1;
    assert(virtqueue_split_enable_interrupts(&vq) == VIRTIO_TRUE);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);

    /*
     * EVENT_IDX suppression: uses used_event and also keeps NO_INTERRUPT in sync
     * for best-effort compatibility.
     */
    test_os_ctx_init(&os_ctx);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);
    assert(vq.used_event != NULL);
    assert(vq.avail_event != NULL);

    /* Prime the simulated device state. */
    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    sg.addr = 0x200000u;
    sg.len = 512;
    sg.device_writes = VIRTIO_FALSE;

    assert(virtqueue_split_add_sg(&vq, &sg, 1, (void *)(uintptr_t)0x1u, VIRTIO_FALSE, &head) == VIRTIO_OK);
    (void)head;
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    sim_process(&sim);

    cookie_out = NULL;
    used_len = 0;
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == (void *)(uintptr_t)0x1u);
    assert(used_len == sg.len);
    assert(vq.last_used_idx == 1);

    virtqueue_split_disable_interrupts(&vq);
    assert((vq.avail->flags & VRING_AVAIL_F_NO_INTERRUPT) != 0);
    assert(*vq.used_event == 0); /* last_used_idx - 1 */

    assert(virtqueue_split_enable_interrupts(&vq) == VIRTIO_FALSE);
    assert((vq.avail->flags & VRING_AVAIL_F_NO_INTERRUPT) == 0);
    assert(*vq.used_event == vq.last_used_idx);

    /* Pre-existing completion -> enable returns TRUE (needs drain). */
    vq.used->idx = (uint16_t)(vq.last_used_idx + 1u);
    assert(virtqueue_split_enable_interrupts(&vq) == VIRTIO_TRUE);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void validate_queue(const virtqueue_split_t *vq)
{
    uint8_t stack_seen[256];
    uint8_t *seen;
    uint16_t idx;
    uint16_t count;
    uint16_t i;

    assert(vq != NULL);
    assert(vq->queue_size != 0);
    assert(vq->num_free <= vq->queue_size);

    if (vq->queue_size <= (uint16_t)VIRTIO_ARRAY_SIZE(stack_seen)) {
        seen = stack_seen;
        memset(seen, 0, vq->queue_size);
    } else {
        seen = (uint8_t *)calloc(vq->queue_size, 1);
        assert(seen != NULL);
    }

    idx = vq->free_head;
    count = 0;
    while (idx != 0xffffu) {
        assert(idx < vq->queue_size);
        assert(seen[idx] == 0);
        seen[idx] = 1;
        idx = vq->desc[idx].next;
        count++;
        assert(count <= vq->queue_size);
    }
    assert(count == vq->num_free);

    for (i = 0; i < vq->queue_size; i++) {
        if (vq->cookies[i] != NULL) {
            assert(seen[i] == 0);
        }
    }

    if (seen != stack_seen) {
        free(seen);
    }
}

static void test_wraparound_event_idx(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    for (i = 0; i < 70000u; i++) {
        virtio_sg_entry_t sg;
        uint16_t head;
        void *cookie_in;
        void *cookie_out;
        uint32_t used_len;

        sg.addr = 0x220000u + ((uint64_t)i * 0x100u);
        sg.len = 512;
        sg.device_writes = VIRTIO_FALSE;

        cookie_in = (void *)(uintptr_t)(i + 1u);
        assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
        assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);

        sim_process(&sim);
        assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
        assert(cookie_out == cookie_in);
        assert(used_len == sg.len);

        assert(vq.num_free == vq.queue_size);
        validate_queue(&vq);
    }

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_event_idx_notify_suppression(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    uintptr_t expected[256];
    size_t exp_head;
    size_t exp_tail;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 32, 4096, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                32,
                                4096,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 4;

    exp_head = 0;
    exp_tail = 0;

    /* Prime avail_event for event idx batching. */
    assert(vq.avail_event != NULL);
    *vq.avail_event = (uint16_t)(sim.notify_batch - 1u);

    for (i = 0; i < 100u; i++) {
        virtio_sg_entry_t sg;
        uint16_t head;
        void *cookie;

        sg.addr = 0x500000u + ((uint64_t)i * 0x1000u);
        sg.len = 512;
        sg.device_writes = VIRTIO_FALSE;

        cookie = (void *)(uintptr_t)(i + 1u);
        assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie, VIRTIO_FALSE, &head) == VIRTIO_OK);
        expected[exp_tail++ % VIRTIO_ARRAY_SIZE(expected)] = (uintptr_t)cookie;

        if (virtqueue_split_kick_prepare(&vq) != VIRTIO_FALSE) {
            sim_process(&sim);
        }

        /* Drain any completions the simulated device has produced. */
        for (;;) {
            void *out_cookie;
            uint32_t out_len;
            if (virtqueue_split_pop_used(&vq, &out_cookie, &out_len) == VIRTIO_FALSE) {
                break;
            }
            assert(exp_head != exp_tail);
            assert((uintptr_t)out_cookie == expected[exp_head % VIRTIO_ARRAY_SIZE(expected)]);
            assert(out_len == sg.len);
            exp_head++;
        }

        validate_queue(&vq);
    }

    /* Drain remaining submissions. */
    sim_process(&sim);
    for (;;) {
        void *out_cookie;
        uint32_t out_len;
        if (virtqueue_split_pop_used(&vq, &out_cookie, &out_len) == VIRTIO_FALSE) {
            break;
        }
        assert(exp_head != exp_tail);
        assert((uintptr_t)out_cookie == expected[exp_head % VIRTIO_ARRAY_SIZE(expected)]);
        assert(out_len == 512);
        exp_head++;
    }
    assert(exp_head == exp_tail);
    assert(vq.num_free == vq.queue_size);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static uint32_t sim_sum_desc_len(vring_device_sim_t *sim, uint16_t head)
{
    virtqueue_split_t *vq;
    test_os_ctx_t *os_ctx;
    uint32_t sum;

    vq = sim->vq;
    os_ctx = (test_os_ctx_t *)vq->os_ctx;

    if (head >= vq->queue_size) {
        return 0;
    }

    sum = 0;
    if ((vq->desc[head].flags & VRING_DESC_F_INDIRECT) != 0) {
        uint16_t n;
        uint16_t i;
        vring_desc_t *table;

        n = (uint16_t)(vq->desc[head].len / sizeof(vring_desc_t));
        table = (vring_desc_t *)test_os_phys_to_virt(os_ctx, vq->desc[head].addr);
        assert(table != NULL);
        assert(n != 0);

        for (i = 0; i < n; i++) {
            sum += table[i].len;
            if ((table[i].flags & VRING_DESC_F_NEXT) == 0) {
                break;
            }
        }
        return sum;
    }

    {
        uint16_t idx;
        uint16_t limit;
        idx = head;
        limit = vq->queue_size;
        while (limit-- != 0) {
            vring_desc_t *d;
            d = &vq->desc[idx];
            sum += d->len;
            if ((d->flags & VRING_DESC_F_NEXT) == 0) {
                break;
            }
            idx = d->next;
            if (idx >= vq->queue_size) {
                break;
            }
        }
    }

    return sum;
}

static void sim_process(vring_device_sim_t *sim)
{
    virtqueue_split_t *vq;
    uint16_t avail_idx;

    vq = sim->vq;
    avail_idx = vq->avail->idx;
    while (sim->last_avail_idx != avail_idx) {
        uint16_t slot;
        uint16_t head;
        uint16_t used_slot;
        uint32_t len;

        slot = (uint16_t)(sim->last_avail_idx % vq->queue_size);
        head = vq->avail->ring[slot];

        len = sim_sum_desc_len(sim, head);
        used_slot = (uint16_t)(vq->used->idx % vq->queue_size);
        vq->used->ring[used_slot].id = head;
        vq->used->ring[used_slot].len = len;
        vq->used->idx++;

        sim->last_avail_idx++;
    }

    if (vq->event_idx != VIRTIO_FALSE && vq->avail_event != NULL) {
        *vq->avail_event = (uint16_t)(sim->last_avail_idx + (sim->notify_batch - 1u));
    }
}

static void test_wraparound(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    for (i = 0; i < 70000u; i++) {
        virtio_sg_entry_t sg;
        uint16_t head;
        void *cookie_in;
        void *cookie_out;
        uint32_t used_len;

        sg.addr = 0x200000u + ((uint64_t)i * 0x100u);
        sg.len = 512;
        sg.device_writes = VIRTIO_FALSE;

        cookie_in = (void *)(uintptr_t)(i + 1u);
        assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
        assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);

        sim_process(&sim);
        assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
        assert(cookie_out == cookie_in);
        assert(used_len == sg.len);

        assert(vq.num_free == vq.queue_size);
        validate_queue(&vq);
    }

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_small_queue_align(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg;
    uint16_t head;
    void *cookie_in;
    void *cookie_out;
    uint32_t used_len;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    /* Modern split rings only require 4-byte alignment for the used ring. */
    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4, VIRTIO_FALSE, &ring) == VIRTIO_OK);

    /* Descriptor table still requires 16-byte alignment. */
    assert(((uintptr_t)ring.vaddr & 0xFu) == 0);
    assert((ring.paddr & 0xFu) == 0);

    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    sg.addr = 0x200000u;
    sg.len = 512;
    sg.device_writes = VIRTIO_FALSE;

    cookie_in = (void *)(uintptr_t)0x1u;
    assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);

    sim_process(&sim);

    cookie_out = NULL;
    used_len = 0;
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == sg.len);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_event_idx_ring_size_and_kick(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    const uint16_t qsz = 8;
    const uint32_t align = 4;
    size_t expected_ring_bytes;
    virtio_sg_entry_t sg;
    uint16_t head;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, qsz, align, VIRTIO_TRUE, &ring) == VIRTIO_OK);

    /*
     * Validate ring sizing for EVENT_IDX-enabled split rings.
     *
     * For queue_align=4, enabling EVENT_IDX changes both the avail and used ring
     * sizes by +2 bytes and may shift the used ring offset due to alignment.
     */
    expected_ring_bytes =
        /* desc[] */ (sizeof(vring_desc_t) * (size_t)qsz) +
        /* avail (flags+idx + ring[] + used_event) */ ((sizeof(uint16_t) * 2u) + (sizeof(uint16_t) * (size_t)qsz) + sizeof(uint16_t)) +
        /* used ring alignment padding */ 0;
    expected_ring_bytes = (expected_ring_bytes + (align - 1u)) & ~(size_t)(align - 1u);
    expected_ring_bytes =
        /* used (flags+idx + ring[] + avail_event) */ expected_ring_bytes +
        ((sizeof(uint16_t) * 2u) + (sizeof(vring_used_elem_t) * (size_t)qsz) + sizeof(uint16_t));
    expected_ring_bytes = (expected_ring_bytes + (align - 1u)) & ~(size_t)(align - 1u);

    assert(ring.size == expected_ring_bytes);

    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                qsz,
                                align,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    assert(vq.event_idx != VIRTIO_FALSE);
    assert(vq.used_event != NULL);
    assert(vq.avail_event != NULL);
    assert(vq.used_event == &vq.avail->ring[qsz]);
    assert(vq.avail_event == (uint16_t *)(void *)&vq.used->ring[qsz]);

    /*
     * Kick suppression sanity check:
     * If the device requests notifications every 4 new available entries
     * (avail_event=3, old=0), virtqueue_split_kick_prepare should only request a
     * kick on the 4th submission.
     */
    *vq.avail_event = 3;

    sg.addr = 0x200000u;
    sg.len = 512;
    sg.device_writes = VIRTIO_FALSE;

    for (i = 0; i < 3; i++) {
        assert(virtqueue_split_add_sg(&vq, &sg, 1, (void *)(uintptr_t)(i + 1u), VIRTIO_FALSE, &head) == VIRTIO_OK);
        assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_FALSE);
        /* last_kick_avail tracks the last observed avail index, even if no kick is needed. */
        assert(vq.last_kick_avail == vq.avail_idx);
    }

    assert(virtqueue_split_add_sg(&vq, &sg, 1, (void *)(uintptr_t)0x4u, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    assert(vq.last_kick_avail == vq.avail_idx);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_event_idx_kick_wraparound_math(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    const uint16_t qsz = 8;
    const uint32_t align = 4;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, qsz, align, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                qsz,
                                align,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);
    assert(vq.avail_event != NULL);

    /*
     * Validate vring_need_event() wrap-around behaviour via kick_prepare().
     *
     * Simulate old_idx close to 0xffff and new_idx after wrapping to 0x0001.
     */
    vq.avail_idx = 1;
    vq.last_kick_avail = 0xfffeu;
    *vq.avail_event = 0;
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    assert(vq.last_kick_avail == 1);

    vq.avail_idx = 1;
    vq.last_kick_avail = 0xfffeu;
    *vq.avail_event = 2;
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_FALSE);
    assert(vq.last_kick_avail == 1);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_used_no_notify_kick_suppression(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    virtio_sg_entry_t sg;
    uint16_t head;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    sg.addr = 0x700000u;
    sg.len = 1;
    sg.device_writes = VIRTIO_FALSE;

    /* Device requests no notifications. */
    vq.used->flags = VRING_USED_F_NO_NOTIFY;

    assert(virtqueue_split_add_sg(&vq, &sg, 1, (void *)(uintptr_t)0x1u, VIRTIO_FALSE, &head) == VIRTIO_OK);
    (void)head;
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_FALSE);
    /* last_kick_avail tracks the last observed avail index even when suppressed. */
    assert(vq.last_kick_avail == vq.avail_idx);

    /* If the device later clears NO_NOTIFY, the next submission should kick. */
    vq.used->flags = 0;
    assert(virtqueue_split_add_sg(&vq, &sg, 1, (void *)(uintptr_t)0x2u, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    assert(vq.last_kick_avail == vq.avail_idx);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_invalid_queue_align(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_ring_size(8, 2, VIRTIO_FALSE) == 0);
    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 2, VIRTIO_FALSE, &ring) == VIRTIO_ERR_INVAL);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                2,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_ERR_INVAL);

    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_invalid_used_id(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    void *cookie;
    uint32_t used_len;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    assert(virtqueue_split_get_error_flags(&vq) == 0);

    /* Inject a malformed used entry without any in-flight descriptors. */
    vq.used->ring[0].id = (uint32_t)vq.queue_size + 1u;
    vq.used->ring[0].len = 0xdeadbeefu;
    vq.used->idx = 1;

    cookie = (void *)(uintptr_t)0x1111u;
    used_len = 0xbeefu;
    assert(virtqueue_split_pop_used(&vq, &cookie, &used_len) == VIRTIO_TRUE);
    assert(cookie == NULL);
    assert(used_len == 0);
    assert(vq.last_used_idx == 1);

    assert((virtqueue_split_get_error_flags(&vq) & VIRTQUEUE_SPLIT_ERR_INVALID_USED_ID) != 0);
    virtqueue_split_clear_error_flags(&vq);
    assert(virtqueue_split_get_error_flags(&vq) == 0);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_indirect_descriptors(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg[10];
    uint16_t head;
    void *cookie_in;
    void *cookie_out;
    uint32_t used_len;
    uint32_t i;
    uint32_t expected_sum;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_TRUE,
                                32) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    expected_sum = 0;
    for (i = 0; i < 10; i++) {
        sg[i].addr = 0x300000u + ((uint64_t)i * 0x1000u);
        sg[i].len = 128u + i;
        sg[i].device_writes = (i & 1u) ? VIRTIO_TRUE : VIRTIO_FALSE;
        expected_sum += sg[i].len;
    }

    cookie_in = (void *)(uintptr_t)0xabcdu;
    assert(virtqueue_split_add_sg(&vq, sg, 10, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);

    assert((vq.desc[head].flags & VRING_DESC_F_INDIRECT) != 0);
    assert(vq.desc[head].addr == vq.indirect[head].table.paddr);
    assert(vq.desc[head].len == 10u * (uint32_t)sizeof(vring_desc_t));

    /* Validate the indirect table contents. */
    {
        vring_desc_t *table;
        table = (vring_desc_t *)vq.indirect[head].table.vaddr;
        assert(table != NULL);
        for (i = 0; i < 10; i++) {
            uint16_t flags;
            flags = 0;
            if (sg[i].device_writes != VIRTIO_FALSE) {
                flags |= VRING_DESC_F_WRITE;
            }
            if (i + 1u < 10u) {
                flags |= VRING_DESC_F_NEXT;
                assert(table[i].next == (uint16_t)(i + 1u));
            } else {
                assert((table[i].flags & VRING_DESC_F_NEXT) == 0);
            }
            assert(table[i].addr == sg[i].addr);
            assert(table[i].len == sg[i].len);
            assert((table[i].flags & (VRING_DESC_F_WRITE | VRING_DESC_F_NEXT)) == flags);
        }
    }

    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == expected_sum);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    /* Direct chain with sg_count > queue_size should fail. */
    {
        virtio_sg_entry_t too_many[9];
        for (i = 0; i < 9; i++) {
            too_many[i].addr = 0x400000u + i;
            too_many[i].len = 1;
            too_many[i].device_writes = VIRTIO_FALSE;
        }
        assert(virtqueue_split_add_sg(&vq, too_many, 9, (void *)1, VIRTIO_FALSE, &head) == VIRTIO_ERR_RANGE);
        assert(vq.num_free == vq.queue_size);
        validate_queue(&vq);
    }

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_reset(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg[3];
    uint16_t head;
    void *cookie_in;
    void *cookie_out;
    uint32_t used_len;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4096, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4096,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_TRUE,
                                8) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    for (i = 0; i < 3; i++) {
        sg[i].addr = 0x900000u + (uint64_t)i * 0x1000u;
        sg[i].len = 128u + i;
        sg[i].device_writes = (i == 2) ? VIRTIO_TRUE : VIRTIO_FALSE;
    }

    cookie_in = (void *)(uintptr_t)0x1234u;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);

    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == sg[0].len + sg[1].len + sg[2].len);

    /* Add one more in-flight request, then reset the queue (no device access). */
    cookie_in = (void *)(uintptr_t)0x5678u;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(vq.num_free == 7);

    virtqueue_split_reset(&vq);
    sim.last_avail_idx = 0;

    assert(vq.avail_idx == 0);
    assert(vq.last_used_idx == 0);
    assert(vq.last_kick_avail == 0);
    assert(vq.num_free == vq.queue_size);
    assert(vq.free_head == 0);
    assert(vq.avail->idx == 0);
    assert(vq.used->idx == 0);

    for (i = 0; i < vq.queue_size; i++) {
        assert(vq.cookies[i] == NULL);
    }

    /* Ensure the queue remains usable after reset. */
    cookie_in = (void *)(uintptr_t)0x9abcu;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_reset_queue_align4(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg[3];
    uint16_t head;
    void *cookie_in;
    void *cookie_out;
    uint32_t used_len;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    /* Modern split rings use 4-byte used-ring alignment; descriptor table still needs 16. */
    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4, VIRTIO_FALSE, &ring) == VIRTIO_OK);
    assert(((uintptr_t)ring.vaddr & 0xFu) == 0);
    assert((ring.paddr & 0xFu) == 0);

    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4,
                                &ring,
                                VIRTIO_FALSE,
                                VIRTIO_TRUE,
                                8) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    for (i = 0; i < 3; i++) {
        sg[i].addr = 0x900000u + (uint64_t)i * 0x1000u;
        sg[i].len = 128u + i;
        sg[i].device_writes = (i == 2) ? VIRTIO_TRUE : VIRTIO_FALSE;
    }

    cookie_in = (void *)(uintptr_t)0x1234u;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);

    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == sg[0].len + sg[1].len + sg[2].len);

    /* Add one more in-flight request, then reset the queue (no device access). */
    cookie_in = (void *)(uintptr_t)0x5678u;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(vq.num_free == 7);

    virtqueue_split_reset(&vq);
    sim.last_avail_idx = 0;

    assert(vq.avail_idx == 0);
    assert(vq.last_used_idx == 0);
    assert(vq.last_kick_avail == 0);
    assert(vq.num_free == vq.queue_size);
    assert(vq.free_head == 0);
    assert(vq.avail->idx == 0);
    assert(vq.used->idx == 0);

    for (i = 0; i < vq.queue_size; i++) {
        assert(vq.cookies[i] == NULL);
    }

    /* Ensure the queue remains usable after reset. */
    cookie_in = (void *)(uintptr_t)0x9abcu;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_reset_event_idx_queue_align4(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg[3];
    uint16_t head;
    void *cookie_in;
    void *cookie_out;
    uint32_t used_len;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(((uintptr_t)ring.vaddr & 0xFu) == 0);
    assert((ring.paddr & 0xFu) == 0);

    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_TRUE,
                                8) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 2;

    for (i = 0; i < 3; i++) {
        sg[i].addr = 0x900000u + (uint64_t)i * 0x1000u;
        sg[i].len = 128u + i;
        sg[i].device_writes = (i == 2) ? VIRTIO_TRUE : VIRTIO_FALSE;
    }

    cookie_in = (void *)(uintptr_t)0x1234u;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);

    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == sg[0].len + sg[1].len + sg[2].len);

    /* Ensure the device-written event index is non-zero so reset clears it. */
    assert(vq.avail_event != NULL);
    assert(*vq.avail_event != 0);

    /* Ensure used_event is cleared by reset too (it is driver-written). */
    assert(vq.used_event != NULL);
    *vq.used_event = 0xbeefu;

    /* Add one more in-flight request, then reset the queue (no device access). */
    cookie_in = (void *)(uintptr_t)0x5678u;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(vq.num_free == 7);

    virtqueue_split_reset(&vq);
    sim.last_avail_idx = 0;

    assert(vq.avail_idx == 0);
    assert(vq.last_used_idx == 0);
    assert(vq.last_kick_avail == 0);
    assert(vq.num_free == vq.queue_size);
    assert(vq.free_head == 0);
    assert(vq.avail->idx == 0);
    assert(vq.used->idx == 0);

    assert(*vq.used_event == 0);
    assert(*vq.avail_event == 0);

    for (i = 0; i < vq.queue_size; i++) {
        assert(vq.cookies[i] == NULL);
    }

    /* Ensure the queue remains usable after reset. */
    cookie_in = (void *)(uintptr_t)0x9abcu;
    assert(virtqueue_split_add_sg(&vq, sg, 3, cookie_in, VIRTIO_TRUE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_reset_invalid_queue_align_fallback(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg;
    uint16_t head;
    void *cookie_in;
    void *cookie_out;
    uint32_t used_len;
    uint32_t i;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    sg.addr = 0x200000u;
    sg.len = 512;
    sg.device_writes = VIRTIO_FALSE;

    cookie_in = (void *)(uintptr_t)0x1234u;
    assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == sg.len);

    /* Ensure we have an in-flight cookie when we reset. */
    cookie_in = (void *)(uintptr_t)0x5678u;
    assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(vq.num_free == 7);

    /* Corrupt queue_align to force virtqueue_split_ring_size() == 0 in reset(). */
    vq.queue_align = 3;

    assert(vq.used_event != NULL);
    assert(vq.avail_event != NULL);
    *vq.used_event = 0xbeefu;
    *vq.avail_event = 0xbeefu;

    virtqueue_split_reset(&vq);
    sim.last_avail_idx = 0;

    assert(vq.avail_idx == 0);
    assert(vq.last_used_idx == 0);
    assert(vq.last_kick_avail == 0);
    assert(vq.num_free == vq.queue_size);
    assert(vq.free_head == 0);
    assert(vq.avail->idx == 0);
    assert(vq.used->idx == 0);
    assert(*vq.used_event == 0);
    assert(*vq.avail_event == 0);

    for (i = 0; i < vq.queue_size; i++) {
        assert(vq.cookies[i] == NULL);
    }

    /* Restore alignment and ensure the queue remains usable after the fallback reset path. */
    vq.queue_align = 4;

    cookie_in = (void *)(uintptr_t)0x9abcu;
    assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == sg.len);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_reset_ring_size_overflow_fallback(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    virtio_sg_entry_t sg;
    uint16_t head;
    void *cookie_in;
    void *cookie_out;
    uint32_t used_len;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 8, 4, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                8,
                                4,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_FALSE,
                                0) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 1;

    sg.addr = 0x200000u;
    sg.len = 512;
    sg.device_writes = VIRTIO_FALSE;

    /* Ensure we have an in-flight cookie when we reset. */
    cookie_in = (void *)(uintptr_t)0x1234u;
    assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(vq.num_free == 7);

    /*
     * Corrupt queue_align to a valid-but-wrong value that makes
     * virtqueue_split_ring_size() compute a size larger than the original
     * allocation. Reset must not blindly memset past the ring buffer.
     */
    vq.queue_align = 4096;

    virtqueue_split_reset(&vq);
    sim.last_avail_idx = 0;

    assert(vq.avail_idx == 0);
    assert(vq.last_used_idx == 0);
    assert(vq.last_kick_avail == 0);
    assert(vq.num_free == vq.queue_size);
    assert(vq.free_head == 0);
    assert(vq.avail->idx == 0);
    assert(vq.used->idx == 0);

    for (uint32_t i = 0; i < vq.queue_size; i++) {
        assert(vq.cookies[i] == NULL);
    }

    /* Restore and ensure the queue remains usable after the fallback reset path. */
    vq.queue_align = 4;

    cookie_in = (void *)(uintptr_t)0x9abcu;
    assert(virtqueue_split_add_sg(&vq, &sg, 1, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);
    assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
    sim_process(&sim);
    assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
    assert(cookie_out == cookie_in);
    assert(used_len == sg.len);

    assert(vq.num_free == vq.queue_size);
    validate_queue(&vq);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_fuzz(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    vring_device_sim_t sim;
    uintptr_t expected[1024];
    size_t exp_head;
    size_t exp_tail;
    uint32_t iter;
    uint32_t next_cookie;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, 32, 4096, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                32,
                                4096,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_TRUE,
                                64) == VIRTIO_OK);

    memset(&sim, 0, sizeof(sim));
    sim.vq = &vq;
    sim.notify_batch = 4;

    exp_head = 0;
    exp_tail = 0;
    next_cookie = 1;

    /* Prime avail_event for event idx batching. */
    if (vq.avail_event != NULL) {
        *vq.avail_event = (uint16_t)(sim.notify_batch - 1u);
    }

    for (iter = 0; iter < 20000u; iter++) {
        uint32_t r;

        r = rng_next();
        if ((r & 3u) != 0) {
            virtio_sg_entry_t sg[64];
            uint16_t sg_count;
            virtio_bool_t use_indirect;
            uint16_t head;
            uint32_t i;
            int rc;
            void *cookie;

            if ((r & 0x20u) != 0) {
                sg_count = (uint16_t)((r % 32u) + 1u);
                use_indirect = VIRTIO_TRUE;
            } else {
                sg_count = (uint16_t)((r % 4u) + 1u);
                use_indirect = (r & 0x10u) ? VIRTIO_TRUE : VIRTIO_FALSE;
            }

            for (i = 0; i < sg_count; i++) {
                sg[i].addr = 0x800000u + ((uint64_t)next_cookie << 12) + (uint64_t)i * 0x100u;
                sg[i].len = (uint32_t)((rng_next() % 2048u) + 1u);
                sg[i].device_writes = (rng_next() & 1u) ? VIRTIO_TRUE : VIRTIO_FALSE;
            }

            cookie = (void *)(uintptr_t)next_cookie++;
            rc = virtqueue_split_add_sg(&vq, sg, sg_count, cookie, use_indirect, &head);
            if (rc == VIRTIO_OK) {
                expected[exp_tail % VIRTIO_ARRAY_SIZE(expected)] = (uintptr_t)cookie;
                exp_tail++;

                if (virtqueue_split_kick_prepare(&vq) != VIRTIO_FALSE) {
                    sim_process(&sim);
                }
            } else {
                void *out_cookie;
                uint32_t out_len;
                /* Make progress by processing and completing one entry if possible. */
                sim_process(&sim);
                if (virtqueue_split_pop_used(&vq, &out_cookie, &out_len) != VIRTIO_FALSE) {
                    (void)out_len;
                    assert(exp_head != exp_tail);
                    assert((uintptr_t)out_cookie == expected[exp_head % VIRTIO_ARRAY_SIZE(expected)]);
                    exp_head++;
                }
            }
        } else {
            void *out_cookie;
            uint32_t out_len;
            if (virtqueue_split_pop_used(&vq, &out_cookie, &out_len) != VIRTIO_FALSE) {
                (void)out_len;
                assert(exp_head != exp_tail);
                assert((uintptr_t)out_cookie == expected[exp_head % VIRTIO_ARRAY_SIZE(expected)]);
                exp_head++;
            }
        }

        validate_queue(&vq);
    }

    /* Drain. */
    sim_process(&sim);
    for (;;) {
        void *out_cookie;
        uint32_t out_len;
        if (virtqueue_split_pop_used(&vq, &out_cookie, &out_len) == VIRTIO_FALSE) {
            break;
        }
        (void)out_len;
        assert(exp_head != exp_tail);
        assert((uintptr_t)out_cookie == expected[exp_head % VIRTIO_ARRAY_SIZE(expected)]);
        exp_head++;
    }
    assert(exp_head == exp_tail);
    assert(vq.num_free == vq.queue_size);

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

static void test_pci_legacy_integration(void)
{
    test_os_ctx_t os_ctx;
    virtio_os_ops_t os_ops;
    fake_pci_device_t fake;
    test_io_region_t io_region;
    virtio_pci_legacy_device_t dev;
    virtio_dma_buffer_t ring;
    virtqueue_split_t vq;
    uint32_t align;
    uint16_t qsz;
    uint64_t host_features;
    uint64_t driver_features;

    test_os_ctx_init(&os_ctx);
    test_os_get_ops(&os_ops);

    fake_pci_device_init(&fake, &os_ctx, 8, 4096, VIRTIO_TRUE, 1);

    io_region.kind = TEST_IO_REGION_LEGACY_PIO;
    io_region.dev = &fake;
    virtio_pci_legacy_init(&dev, &os_ops, &os_ctx, (uintptr_t)&io_region, VIRTIO_FALSE);
    virtio_pci_legacy_reset(&dev);
    virtio_pci_legacy_add_status(&dev, VIRTIO_STATUS_ACKNOWLEDGE);
    virtio_pci_legacy_add_status(&dev, VIRTIO_STATUS_DRIVER);

    host_features = virtio_pci_legacy_read_device_features(&dev);
    driver_features = host_features & (VIRTIO_RING_F_INDIRECT_DESC | VIRTIO_RING_F_EVENT_IDX);
    virtio_pci_legacy_write_driver_features(&dev, driver_features);
    virtio_pci_legacy_add_status(&dev, VIRTIO_STATUS_FEATURES_OK);

    align = virtio_pci_legacy_get_vring_align();
    qsz = virtio_pci_legacy_get_queue_size(&dev, 0);
    assert(align == 4096);
    assert(qsz == 8);

    assert(virtqueue_split_alloc_ring(&os_ops, &os_ctx, qsz, align, VIRTIO_TRUE, &ring) == VIRTIO_OK);
    assert(virtqueue_split_init(&vq,
                                &os_ops,
                                &os_ctx,
                                0,
                                qsz,
                                align,
                                &ring,
                                VIRTIO_TRUE,
                                VIRTIO_TRUE,
                                32) == VIRTIO_OK);

    assert(virtio_pci_legacy_set_queue_pfn(&dev, 0, ring.paddr) == VIRTIO_OK);

    /*
     * Submit a few requests while exercising:
     *  - EVENT_IDX kick suppression integration (avail_event must be device-written)
     *  - EVENT_IDX interrupt suppression integration (used_event must be driver-written)
     */
    for (uint32_t iter = 0; iter < 3; iter++) {
        virtio_sg_entry_t sg[2];
        uint16_t head;
        void *cookie_in;
        void *cookie_out;
        uint32_t used_len;
        uint8_t isr;

        sg[0].addr = 0x500000u;
        sg[0].len = 16;
        sg[0].device_writes = VIRTIO_FALSE;
        sg[1].addr = 0x600000u;
        sg[1].len = 1;
        sg[1].device_writes = VIRTIO_TRUE;

        cookie_in = (void *)(uintptr_t)(0x1111u + iter);
        assert(virtqueue_split_add_sg(&vq, sg, 2, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);

        assert(virtqueue_split_kick_prepare(&vq) == VIRTIO_TRUE);
        virtio_pci_legacy_notify_queue(&dev, 0);

        isr = virtio_pci_legacy_read_isr_status(&dev);
        if (iter == 1) {
            /* Interrupts were disabled after the first completion. */
            assert((isr & 0x1u) == 0);
        } else {
            assert((isr & 0x1u) != 0);
        }

        assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
        assert(cookie_out == cookie_in);
        assert(used_len == (sg[0].len + sg[1].len));

        if (iter == 0) {
            virtqueue_split_disable_interrupts(&vq);
        } else if (iter == 1) {
            assert(virtqueue_split_enable_interrupts(&vq) == VIRTIO_FALSE);
        }
    }

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

int main(void)
{
    test_ring_size_event_idx();
    test_interrupt_suppression_helpers();
    test_wraparound();
    test_wraparound_event_idx();
    test_small_queue_align();
    test_event_idx_ring_size_and_kick();
    test_event_idx_kick_wraparound_math();
    test_used_no_notify_kick_suppression();
    test_invalid_queue_align();
    test_invalid_used_id();
    test_indirect_descriptors();
    test_reset();
    test_reset_queue_align4();
    test_reset_event_idx_queue_align4();
    test_reset_invalid_queue_align_fallback();
    test_reset_ring_size_overflow_fallback();
    test_event_idx_notify_suppression();
    test_fuzz();
    test_pci_legacy_integration();
    printf("virtio_common_tests: PASS\n");
    return 0;
}
