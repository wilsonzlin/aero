/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtqueue_split.h"

static void virtio_memset(void *dst, int c, size_t n)
{
    uint8_t *p;
    p = (uint8_t *)dst;
    while (n-- != 0) {
        *p++ = (uint8_t)c;
    }
}

static void virtio_wmb(const virtio_os_ops_t *os, void *ctx)
{
    if (os == NULL) {
        return;
    }
    if (os->wmb != NULL) {
        os->wmb(ctx);
    } else if (os->mb != NULL) {
        os->mb(ctx);
    }
}

static void virtio_rmb(const virtio_os_ops_t *os, void *ctx)
{
    if (os == NULL) {
        return;
    }
    if (os->rmb != NULL) {
        os->rmb(ctx);
    } else if (os->mb != NULL) {
        os->mb(ctx);
    }
}

static size_t virtqueue_split_avail_size(uint16_t queue_size, virtio_bool_t event_idx)
{
    size_t size;
    size = sizeof(uint16_t) * 2u; /* flags + idx */
    size += sizeof(uint16_t) * (size_t)queue_size; /* ring[] */
    if (event_idx != VIRTIO_FALSE) {
        size += sizeof(uint16_t); /* used_event */
    }
    return size;
}

static size_t virtqueue_split_used_size(uint16_t queue_size, virtio_bool_t event_idx)
{
    size_t size;
    size = sizeof(uint16_t) * 2u; /* flags + idx */
    size += sizeof(vring_used_elem_t) * (size_t)queue_size; /* ring[] */
    if (event_idx != VIRTIO_FALSE) {
        size += sizeof(uint16_t); /* avail_event */
    }
    return size;
}

size_t virtqueue_split_ring_size(uint16_t queue_size, uint32_t queue_align, virtio_bool_t event_idx)
{
    size_t desc_size;
    size_t avail_off;
    size_t used_off;
    size_t used_size;
    size_t total;

    if (queue_size == 0) {
        return 0;
    }
    if (queue_align == 0 || ((queue_align & (queue_align - 1u)) != 0)) {
        return 0;
    }

    desc_size = sizeof(vring_desc_t) * (size_t)queue_size;
    avail_off = desc_size;
    used_off = virtio_align_up_size(avail_off + virtqueue_split_avail_size(queue_size, event_idx), (size_t)queue_align);
    used_size = virtqueue_split_used_size(queue_size, event_idx);
    total = virtio_align_up_size(used_off + used_size, (size_t)queue_align);
    return total;
}

int virtqueue_split_alloc_ring(const virtio_os_ops_t *os,
                               void *os_ctx,
                               uint16_t queue_size,
                               uint32_t queue_align,
                               virtio_bool_t event_idx,
                               virtio_dma_buffer_t *out_ring)
{
    size_t ring_size;

    if (out_ring == NULL || os == NULL || os->alloc_dma == NULL) {
        return VIRTIO_ERR_INVAL;
    }

    ring_size = virtqueue_split_ring_size(queue_size, queue_align, event_idx);
    if (ring_size == 0) {
        return VIRTIO_ERR_INVAL;
    }

    virtio_memset(out_ring, 0, sizeof(*out_ring));
    if (os->alloc_dma(os_ctx, ring_size, (size_t)queue_align, out_ring) == VIRTIO_FALSE) {
        return VIRTIO_ERR_NOMEM;
    }

    return VIRTIO_OK;
}

void virtqueue_split_free_ring(const virtio_os_ops_t *os, void *os_ctx, virtio_dma_buffer_t *ring)
{
    if (ring == NULL || os == NULL || os->free_dma == NULL) {
        return;
    }
    if (ring->vaddr == NULL || ring->size == 0) {
        return;
    }

    os->free_dma(os_ctx, ring);
    virtio_memset(ring, 0, sizeof(*ring));
}

static virtio_bool_t virtqueue_split_need_event(uint16_t event, uint16_t new_idx, uint16_t old_idx)
{
    /* vring_need_event() from the virtio spec / Linux: */
    return ((uint16_t)(new_idx - event - 1u) < (uint16_t)(new_idx - old_idx)) ? VIRTIO_TRUE : VIRTIO_FALSE;
}

static void virtqueue_split_free_chain(virtqueue_split_t *vq, uint16_t head)
{
    uint16_t idx;
    uint16_t safety;

    if (vq == NULL || vq->desc == NULL) {
        return;
    }

    idx = head;
    safety = vq->queue_size;

    while (safety-- != 0) {
        vring_desc_t *d;
        uint16_t next;
        virtio_bool_t has_next;

        if (idx >= vq->queue_size) {
            if (vq->os != NULL && vq->os->log != NULL) {
                vq->os->log(vq->os_ctx, "virtqueue_split: used id out of range: %u", (unsigned)idx);
            }
            return;
        }

        d = &vq->desc[idx];
        next = d->next;
        has_next = (d->flags & VRING_DESC_F_NEXT) ? VIRTIO_TRUE : VIRTIO_FALSE;
        if (d->flags & VRING_DESC_F_INDIRECT) {
            /* Indirect uses only the head descriptor in the main table. */
            has_next = VIRTIO_FALSE;
        }

        /* Clear descriptor and push back to free list. */
        virtio_memset(d, 0, sizeof(*d));
        d->next = vq->free_head;
        vq->free_head = idx;
        vq->num_free++;

        if (has_next == VIRTIO_FALSE) {
            return;
        }

        idx = next;
    }

    if (vq->os != NULL && vq->os->log != NULL) {
        vq->os->log(vq->os_ctx, "virtqueue_split: descriptor chain loop detected (head=%u)", (unsigned)head);
    }
}

int virtqueue_split_init(virtqueue_split_t *vq,
                         const virtio_os_ops_t *os,
                         void *os_ctx,
                         uint16_t queue_index,
                         uint16_t queue_size,
                         uint32_t queue_align,
                         const virtio_dma_buffer_t *ring_dma,
                         virtio_bool_t event_idx,
                         virtio_bool_t indirect_desc,
                         uint16_t indirect_max_desc)
{
    size_t desc_size;
    size_t avail_off;
    size_t used_off;
    size_t ring_required;
    uint8_t *base;
    uint16_t i;

    if (vq == NULL || os == NULL || ring_dma == NULL || ring_dma->vaddr == NULL) {
        return VIRTIO_ERR_INVAL;
    }
    if (queue_size == 0 || queue_align == 0 || ((queue_align & (queue_align - 1u)) != 0)) {
        return VIRTIO_ERR_INVAL;
    }

    ring_required = virtqueue_split_ring_size(queue_size, queue_align, event_idx);
    if (ring_required == 0 || ring_dma->size < ring_required) {
        return VIRTIO_ERR_RANGE;
    }
    if ((ring_dma->paddr & ((uint64_t)queue_align - 1u)) != 0) {
        /* Legacy queue base must satisfy QUEUE_ALIGN. */
        return VIRTIO_ERR_RANGE;
    }

    virtio_memset(vq, 0, sizeof(*vq));
    vq->os = os;
    vq->os_ctx = os_ctx;
    vq->queue_index = queue_index;
    vq->queue_size = queue_size;
    vq->queue_align = queue_align;
    vq->ring_dma = *ring_dma;
    vq->event_idx = event_idx;
    vq->indirect_desc = indirect_desc;
    vq->indirect_max_desc = indirect_max_desc;

    base = (uint8_t *)ring_dma->vaddr;
    desc_size = sizeof(vring_desc_t) * (size_t)queue_size;
    avail_off = desc_size;
    used_off = virtio_align_up_size(avail_off + virtqueue_split_avail_size(queue_size, event_idx), (size_t)queue_align);

    vq->desc = (vring_desc_t *)(void *)(base);
    vq->avail = (vring_avail_t *)(void *)(base + avail_off);
    vq->used = (vring_used_t *)(void *)(base + used_off);

    if (event_idx != VIRTIO_FALSE) {
        vq->used_event = &vq->avail->ring[queue_size];
        vq->avail_event = (uint16_t *)(void *)((uint8_t *)&vq->used->ring[queue_size]);
    }

    /* Reset and zero the ring region we own. */
    virtio_memset(ring_dma->vaddr, 0, ring_required);

    /* Init free list. */
    vq->free_head = 0;
    vq->num_free = queue_size;
    for (i = 0; i < queue_size; i++) {
        vq->desc[i].next = (uint16_t)(i + 1u);
    }
    vq->desc[queue_size - 1u].next = 0xffffu;

    if (os->alloc == NULL || os->free == NULL) {
        return VIRTIO_ERR_INVAL;
    }
    vq->cookies = (void **)os->alloc(os_ctx,
                                     sizeof(void *) * (size_t)queue_size,
                                     (virtio_os_alloc_flags_t)(VIRTIO_OS_ALLOC_NONPAGED | VIRTIO_OS_ALLOC_ZERO));
    if (vq->cookies == NULL) {
        return VIRTIO_ERR_NOMEM;
    }

    if (indirect_desc != VIRTIO_FALSE) {
        if (indirect_max_desc == 0 || os->alloc_dma == NULL || os->free_dma == NULL) {
            virtqueue_split_destroy(vq);
            return VIRTIO_ERR_INVAL;
        }

        vq->indirect =
            (virtqueue_split_indirect_t *)os->alloc(os_ctx,
                                                    sizeof(virtqueue_split_indirect_t) * (size_t)queue_size,
                                                    (virtio_os_alloc_flags_t)(VIRTIO_OS_ALLOC_NONPAGED | VIRTIO_OS_ALLOC_ZERO));
        if (vq->indirect == NULL) {
            virtqueue_split_destroy(vq);
            return VIRTIO_ERR_NOMEM;
        }

        for (i = 0; i < queue_size; i++) {
            size_t table_size;
            virtio_bool_t ok;

            table_size = sizeof(vring_desc_t) * (size_t)indirect_max_desc;
            ok = os->alloc_dma(os_ctx, table_size, 16u, &vq->indirect[i].table);
            if (ok == VIRTIO_FALSE) {
                virtqueue_split_destroy(vq);
                return VIRTIO_ERR_NOMEM;
            }
        }
    }

    return VIRTIO_OK;
}

void virtqueue_split_destroy(virtqueue_split_t *vq)
{
    uint16_t i;
    const virtio_os_ops_t *os;
    void *os_ctx;

    if (vq == NULL) {
        return;
    }

    os = vq->os;
    os_ctx = vq->os_ctx;

    if (vq->indirect != NULL && os != NULL && os->free_dma != NULL) {
        for (i = 0; i < vq->queue_size; i++) {
            if (vq->indirect[i].table.vaddr != NULL) {
                os->free_dma(os_ctx, &vq->indirect[i].table);
            }
        }
    }

    if (vq->cookies != NULL && os != NULL && os->free != NULL) {
        os->free(os_ctx, vq->cookies);
    }
    if (vq->indirect != NULL && os != NULL && os->free != NULL) {
        os->free(os_ctx, vq->indirect);
    }

    virtio_memset(vq, 0, sizeof(*vq));
}

int virtqueue_split_add_sg(virtqueue_split_t *vq,
                           const virtio_sg_entry_t *sg,
                           uint16_t sg_count,
                           void *cookie,
                           virtio_bool_t use_indirect,
                           uint16_t *out_head)
{
    uint16_t head;
    uint16_t idx;
    uint16_t i;

    if (vq == NULL || sg == NULL || sg_count == 0 || out_head == NULL) {
        return VIRTIO_ERR_INVAL;
    }

    if (use_indirect != VIRTIO_FALSE) {
        if (vq->indirect_desc == VIRTIO_FALSE) {
            return VIRTIO_ERR_INVAL;
        }
        if (sg_count > vq->indirect_max_desc) {
            return VIRTIO_ERR_RANGE;
        }
        if (vq->num_free < 1u) {
            return VIRTIO_ERR_NOSPC;
        }

        head = vq->free_head;
        if (head >= vq->queue_size) {
            return VIRTIO_ERR_RANGE;
        }
        vq->free_head = vq->desc[head].next;
        vq->num_free--;

        if (vq->cookies[head] != NULL) {
            /* In-flight corruption. */
            virtqueue_split_free_chain(vq, head);
            return VIRTIO_ERR_INVAL;
        }
        vq->cookies[head] = cookie;

        /* Build indirect table. */
        {
            vring_desc_t *table;
            table = (vring_desc_t *)vq->indirect[head].table.vaddr;
            virtio_memset(table, 0, sizeof(vring_desc_t) * (size_t)vq->indirect_max_desc);

            for (i = 0; i < sg_count; i++) {
                uint16_t flags;
                flags = 0;
                if (sg[i].device_writes != VIRTIO_FALSE) {
                    flags |= VRING_DESC_F_WRITE;
                }
                if (i + 1u < sg_count) {
                    flags |= VRING_DESC_F_NEXT;
                }
                table[i].addr = sg[i].addr;
                table[i].len = sg[i].len;
                table[i].flags = flags;
                table[i].next = (uint16_t)(i + 1u);
            }
            table[sg_count - 1u].next = 0;

            vq->desc[head].addr = vq->indirect[head].table.paddr;
            vq->desc[head].len = (uint32_t)sg_count * (uint32_t)sizeof(vring_desc_t);
            vq->desc[head].flags = VRING_DESC_F_INDIRECT;
            vq->desc[head].next = 0;
        }
    } else {
        if (sg_count > vq->queue_size) {
            return VIRTIO_ERR_RANGE;
        }
        if (vq->num_free < sg_count) {
            return VIRTIO_ERR_NOSPC;
        }

        head = vq->free_head;
        idx = head;
        for (i = 0; i < sg_count; i++) {
            vring_desc_t *d;
            uint16_t next;
            uint16_t flags;

            if (idx >= vq->queue_size) {
                return VIRTIO_ERR_RANGE;
            }

            d = &vq->desc[idx];
            next = d->next; /* next free (and next in chain allocation order) */

            flags = 0;
            if (sg[i].device_writes != VIRTIO_FALSE) {
                flags |= VRING_DESC_F_WRITE;
            }
            if (i + 1u < sg_count) {
                flags |= VRING_DESC_F_NEXT;
            } else {
                d->next = 0;
            }

            d->addr = sg[i].addr;
            d->len = sg[i].len;
            d->flags = flags;

            idx = next;
        }

        /* Consume from free list. idx now points to the descriptor after the chain. */
        vq->free_head = idx;
        vq->num_free = (uint16_t)(vq->num_free - sg_count);

        if (vq->cookies[head] != NULL) {
            virtqueue_split_free_chain(vq, head);
            return VIRTIO_ERR_INVAL;
        }
        vq->cookies[head] = cookie;
    }

    /* Publish into avail ring. */
    vq->avail->ring[vq->avail_idx % vq->queue_size] = head;
    vq->avail_idx++;
    virtio_wmb(vq->os, vq->os_ctx);
    vq->avail->idx = vq->avail_idx;

    *out_head = head;
    return VIRTIO_OK;
}

virtio_bool_t virtqueue_split_kick_prepare(virtqueue_split_t *vq)
{
    uint16_t new_idx;

    if (vq == NULL) {
        return VIRTIO_FALSE;
    }

    new_idx = vq->avail_idx;
    if (new_idx == vq->last_kick_avail) {
        return VIRTIO_FALSE;
    }

    if (vq->event_idx != VIRTIO_FALSE && vq->avail_event != NULL) {
        uint16_t event;
        uint16_t old;

        old = vq->last_kick_avail;
        virtio_rmb(vq->os, vq->os_ctx);
        event = *vq->avail_event;
        if (virtqueue_split_need_event(event, new_idx, old) != VIRTIO_FALSE) {
            vq->last_kick_avail = new_idx;
            return VIRTIO_TRUE;
        }
        return VIRTIO_FALSE;
    }

    virtio_rmb(vq->os, vq->os_ctx);
    if ((vq->used->flags & VRING_USED_F_NO_NOTIFY) != 0) {
        return VIRTIO_FALSE;
    }

    vq->last_kick_avail = new_idx;
    return VIRTIO_TRUE;
}

virtio_bool_t virtqueue_split_pop_used(virtqueue_split_t *vq, void **out_cookie, uint32_t *out_len)
{
    uint16_t used_idx;
    vring_used_elem_t elem;
    uint16_t slot;
    uint16_t id;
    void *cookie;

    if (vq == NULL || vq->used == NULL) {
        return VIRTIO_FALSE;
    }

    used_idx = vq->used->idx;
    virtio_rmb(vq->os, vq->os_ctx);

    if (vq->last_used_idx == used_idx) {
        return VIRTIO_FALSE;
    }

    slot = (uint16_t)(vq->last_used_idx % vq->queue_size);
    elem = vq->used->ring[slot];
    vq->last_used_idx++;

    id = (uint16_t)elem.id;
    if (id >= vq->queue_size) {
        if (vq->os != NULL && vq->os->log != NULL) {
            vq->os->log(vq->os_ctx, "virtqueue_split: invalid used id %u", (unsigned)id);
        }
        return VIRTIO_TRUE; /* consumed entry */
    }

    cookie = vq->cookies[id];
    vq->cookies[id] = NULL;
    virtqueue_split_free_chain(vq, id);

    if (out_cookie != NULL) {
        *out_cookie = cookie;
    }
    if (out_len != NULL) {
        *out_len = elem.len;
    }

    return VIRTIO_TRUE;
}
