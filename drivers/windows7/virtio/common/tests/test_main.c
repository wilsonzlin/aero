/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_legacy.h"
#include "virtqueue_split.h"

#include "fake_pci_device.h"
#include "test_os.h"

typedef struct vring_device_sim {
    virtqueue_split_t *vq;
    uint16_t last_avail_idx;
    uint16_t notify_batch;
} vring_device_sim_t;

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

    if (vq->event_idx != VIRTIO_FALSE && vq->used_event != NULL) {
        *vq->used_event = (uint16_t)(sim->last_avail_idx + (sim->notify_batch - 1u));
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

    /* Prime used_event for event idx batching. */
    if (vq.used_event != NULL) {
        *vq.used_event = (uint16_t)(sim.notify_batch - 1u);
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

    virtio_pci_legacy_init(&dev, &os_ops, &os_ctx, (uintptr_t)&fake, VIRTIO_FALSE);
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

    /* Submit one request. */
    {
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

        cookie_in = (void *)(uintptr_t)0x1111u;
        assert(virtqueue_split_add_sg(&vq, sg, 2, cookie_in, VIRTIO_FALSE, &head) == VIRTIO_OK);

        if (virtqueue_split_kick_prepare(&vq) != VIRTIO_FALSE) {
            virtio_pci_legacy_notify_queue(&dev, 0);
        }

        isr = virtio_pci_legacy_read_isr_status(&dev);
        assert((isr & 0x1u) != 0);

        assert(virtqueue_split_pop_used(&vq, &cookie_out, &used_len) == VIRTIO_TRUE);
        assert(cookie_out == cookie_in);
        assert(used_len == (sg[0].len + sg[1].len));
    }

    virtqueue_split_destroy(&vq);
    virtqueue_split_free_ring(&os_ops, &os_ctx, &ring);
}

int main(void)
{
    test_wraparound();
    test_indirect_descriptors();
    test_fuzz();
    test_pci_legacy_integration();
    printf("virtio_common_tests: PASS\n");
    return 0;
}
