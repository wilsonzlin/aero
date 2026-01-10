#ifndef VIRTIO_RING_H_
#define VIRTIO_RING_H_

/*
 * Virtio 1.0 split virtqueue ring definitions (vring).
 *
 * These are spec-accurate layouts for the split ring format. Fields are
 * little-endian on the wire; Windows 7 guests are little-endian, so the module
 * stores native values.
 */

#include "virtio_osdep.h"

/* Virtio feature bits relevant to split virtqueues. */
#define VIRTIO_F_RING_INDIRECT_DESC 28
#define VIRTIO_F_RING_EVENT_IDX 29

/* Descriptor flags. */
#define VIRTQ_DESC_F_NEXT 1
#define VIRTQ_DESC_F_WRITE 2
#define VIRTQ_DESC_F_INDIRECT 4

/* Available ring flags. */
#define VIRTQ_AVAIL_F_NO_INTERRUPT 1

/* Used ring flags. */
#define VIRTQ_USED_F_NO_NOTIFY 1

typedef struct _VIRTQ_DESC {
	UINT64 addr;
	UINT32 len;
	UINT16 flags;
	UINT16 next;
} VIRTQ_DESC;

typedef struct _VIRTQ_AVAIL {
	UINT16 flags;
	UINT16 idx;
	UINT16 ring[];
	/* Optional: UINT16 used_event; (only if VIRTIO_F_RING_EVENT_IDX) */
} VIRTQ_AVAIL;

typedef struct _VIRTQ_USED_ELEM {
	UINT32 id;
	UINT32 len;
} VIRTQ_USED_ELEM;

typedef struct _VIRTQ_USED {
	UINT16 flags;
	UINT16 idx;
	VIRTQ_USED_ELEM ring[];
	/* Optional: UINT16 avail_event; (only if VIRTIO_F_RING_EVENT_IDX) */
} VIRTQ_USED;

VIRTIO_STATIC_ASSERT(sizeof(VIRTQ_DESC) == 16, virtq_desc_size_must_be_16);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_DESC, addr) == 0, virtq_desc_addr_off_0);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_DESC, len) == 8, virtq_desc_len_off_8);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_DESC, flags) == 12, virtq_desc_flags_off_12);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_DESC, next) == 14, virtq_desc_next_off_14);

VIRTIO_STATIC_ASSERT(sizeof(VIRTQ_USED_ELEM) == 8, virtq_used_elem_size_must_be_8);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_USED_ELEM, id) == 0, virtq_used_elem_id_off_0);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_USED_ELEM, len) == 4, virtq_used_elem_len_off_4);

VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_AVAIL, flags) == 0, virtq_avail_flags_off_0);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_AVAIL, idx) == 2, virtq_avail_idx_off_2);

VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_USED, flags) == 0, virtq_used_flags_off_0);
VIRTIO_STATIC_ASSERT(offsetof(VIRTQ_USED, idx) == 2, virtq_used_idx_off_2);

static __inline volatile UINT16 *VirtqAvailUsedEvent(VIRTQ_AVAIL *avail, UINT16 qsz)
{
	return (volatile UINT16 *)&avail->ring[qsz];
}

static __inline volatile UINT16 *VirtqUsedAvailEvent(VIRTQ_USED *used, UINT16 qsz)
{
	return (volatile UINT16 *)&used->ring[qsz];
}

#endif /* VIRTIO_RING_H_ */
