/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Split virtqueue implementation (vring) for virtio-pci legacy/transitional
 * devices.
 *
 * This implements the in-memory ring layout and descriptor management. It does
 * not perform transport-specific operations (like kicking via PCI notify).
 */

#ifndef AERO_VIRTQUEUE_SPLIT_H_
#define AERO_VIRTQUEUE_SPLIT_H_

#include "virtio_os.h"

/* virtio ring feature bits (in the device/driver feature bitmap). */
#define VIRTIO_RING_F_INDIRECT_DESC (1u << 28)
#define VIRTIO_RING_F_EVENT_IDX (1u << 29)

/* Split ring descriptor flags. */
#define VRING_DESC_F_NEXT 1u
#define VRING_DESC_F_WRITE 2u
#define VRING_DESC_F_INDIRECT 4u

/* Split ring avail/used flags. */
#define VRING_AVAIL_F_NO_INTERRUPT 1u
#define VRING_USED_F_NO_NOTIFY 1u

typedef struct vring_desc {
    uint64_t addr;
    uint32_t len;
    uint16_t flags;
    uint16_t next;
} vring_desc_t;

typedef struct vring_avail {
    uint16_t flags;
    uint16_t idx;
    uint16_t ring[1]; /* actual size = queue_size */
} vring_avail_t;

typedef struct vring_used_elem {
    uint32_t id; /* head descriptor index */
    uint32_t len;
} vring_used_elem_t;

typedef struct vring_used {
    uint16_t flags;
    uint16_t idx;
    vring_used_elem_t ring[1]; /* actual size = queue_size */
} vring_used_t;

/* Compile-time layout checks (avoid accidental padding differences). */
typedef char _virtio_static_assert_desc_size[(sizeof(vring_desc_t) == 16u) ? 1 : -1];
typedef char _virtio_static_assert_used_elem_size[(sizeof(vring_used_elem_t) == 8u) ? 1 : -1];

typedef struct virtio_sg_entry {
    uint64_t addr;
    uint32_t len;
    virtio_bool_t device_writes; /* set VRING_DESC_F_WRITE */
} virtio_sg_entry_t;

typedef struct virtqueue_split_indirect {
    virtio_dma_buffer_t table;
} virtqueue_split_indirect_t;

typedef struct virtqueue_split {
    const virtio_os_ops_t *os;
    void *os_ctx;

    uint16_t queue_index;
    uint16_t queue_size;
    uint32_t queue_align;

    virtio_dma_buffer_t ring_dma;

    vring_desc_t *desc;
    vring_avail_t *avail;
    vring_used_t *used;

    /* Event idx pointers (only valid if event_idx == TRUE). */
    uint16_t *used_event;  /* &avail->ring[queue_size] */
    uint16_t *avail_event; /* (uint16_t *)&used->ring[queue_size] */

    /* Shadow indices. */
    uint16_t avail_idx;
    uint16_t last_used_idx;
    uint16_t last_kick_avail;

    /* Descriptor free list. */
    uint16_t free_head;
    uint16_t num_free;

    /* Per-head in-flight tracking. */
    void **cookies; /* array[queue_size] */

    virtqueue_split_indirect_t *indirect; /* array[queue_size] if enabled */
    uint16_t indirect_max_desc;

    virtio_bool_t event_idx;
    virtio_bool_t indirect_desc;
} virtqueue_split_t;

/*
 * Compute the ring buffer size required for a split ring with `queue_size`
 * descriptors, where the used ring is aligned to `queue_align`.
 *
 * `queue_align` must be a power of two (virtio-pci legacy QUEUE_ALIGN).
 */
size_t virtqueue_split_ring_size(uint16_t queue_size, uint32_t queue_align, virtio_bool_t event_idx);

/*
 * Allocate and free a DMA-able ring buffer using the OS shim.
 * Convenience helpers for virtio-pci legacy queue setup.
 */
int virtqueue_split_alloc_ring(const virtio_os_ops_t *os,
                               void *os_ctx,
                               uint16_t queue_size,
                               uint32_t queue_align,
                               virtio_bool_t event_idx,
                               virtio_dma_buffer_t *out_ring);
void virtqueue_split_free_ring(const virtio_os_ops_t *os, void *os_ctx, virtio_dma_buffer_t *ring);

int virtqueue_split_init(virtqueue_split_t *vq,
                         const virtio_os_ops_t *os,
                         void *os_ctx,
                         uint16_t queue_index,
                         uint16_t queue_size,
                         uint32_t queue_align,
                         const virtio_dma_buffer_t *ring_dma,
                         virtio_bool_t event_idx,
                         virtio_bool_t indirect_desc,
                         uint16_t indirect_max_desc);

void virtqueue_split_destroy(virtqueue_split_t *vq);

/*
 * Add a descriptor chain described by `sg` entries and publish it into the
 * avail ring.
 *
 * The returned `out_head` is the head descriptor index that will later appear
 * in the used ring.
 *
 * This function does not notify ("kick") the device; call
 * virtqueue_split_kick_prepare() after batching submissions.
 */
int virtqueue_split_add_sg(virtqueue_split_t *vq,
                           const virtio_sg_entry_t *sg,
                           uint16_t sg_count,
                           void *cookie,
                           virtio_bool_t use_indirect,
                           uint16_t *out_head);

/*
 * Decide whether a notify (kick) is required based on negotiated ring
 * features (event idx or VRING_USED_F_NO_NOTIFY).
 */
virtio_bool_t virtqueue_split_kick_prepare(virtqueue_split_t *vq);

/*
 * Pop one used completion if available.
 *
 * Returns VIRTIO_TRUE if a completion was popped, VIRTIO_FALSE if none.
 * On success, `*out_cookie` and `*out_len` are set.
 */
virtio_bool_t virtqueue_split_pop_used(virtqueue_split_t *vq, void **out_cookie, uint32_t *out_len);

#endif /* AERO_VIRTQUEUE_SPLIT_H_ */
