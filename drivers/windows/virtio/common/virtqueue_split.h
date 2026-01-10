#ifndef VIRTQUEUE_SPLIT_H_
#define VIRTQUEUE_SPLIT_H_

/*
 * Virtio 1.0 split virtqueue implementation for Windows 7 guest drivers.
 *
 * Model:
 *  - VirtqSplitAddBuffer() builds a descriptor chain (direct or indirect) and
 *    returns the head descriptor index.
 *  - VirtqSplitPublish() publishes that head to the available ring, performing
 *    the required write barrier before updating avail->idx.
 *  - Drivers can batch multiple publishes; VirtqSplitKickPrepare() uses
 *    vq->num_added to apply notification suppression (event-idx or NO_NOTIFY).
 *    After writing the transport-specific notify register, call
 *    VirtqSplitKickCommit() to reset the batching bookkeeping.
 */

#include "virtio_osdep.h"
#include "virtio_ring.h"

#define VIRTQ_SPLIT_NO_DESC ((UINT16)0xFFFFu)

typedef struct _VIRTQ_SG {
	UINT64 addr;
	UINT32 len;
	BOOLEAN write; /* TRUE if device writes to this buffer */
} VIRTQ_SG;

typedef struct _VIRTQ_SPLIT {
	/* Negotiated queue properties */
	UINT16 qsz;
	BOOLEAN event_idx;
	BOOLEAN indirect;
	UINT32 ring_align;

	/* Ring memory (DMA) */
	void *ring_va;
	UINT64 ring_pa;

	VIRTQ_DESC *desc;
	VIRTQ_AVAIL *avail;
	VIRTQ_USED *used;

	UINT64 desc_pa;
	UINT64 avail_pa;
	UINT64 used_pa;

	/* Driver-side indices */
	UINT16 avail_idx; /* shadow of avail->idx */
	UINT16 last_used_idx;

	/* Descriptor free list */
	UINT16 free_head;
	UINT16 num_free;

	/* Buffers published since last kick commit */
	UINT16 num_added;

	/* Per-head metadata (indexed by head descriptor index) */
	void **cookies;
	UINT16 *head_indirect; /* table index, or VIRTQ_SPLIT_NO_DESC */

	/* Indirect descriptor table pool (optional) */
	VIRTQ_DESC *indirect_pool_va;
	UINT64 indirect_pool_pa;
	UINT16 indirect_table_count;
	UINT16 indirect_max_desc;
	UINT16 indirect_free_head;
	UINT16 indirect_num_free;
	UINT16 indirect_threshold; /* above this SG count, prefer indirect */
	UINT32 indirect_table_stride;

	/*
	 * Flexible storage for cookies/head_indirect.
	 *
	 * Callers must allocate the VIRTQ_SPLIT with enough trailing space for the
	 * per-descriptor metadata:
	 *   void*  cookies[qsz]
	 *   UINT16 head_indirect[qsz]
	 *
	 * Use VirtqSplitStateSize(qsz) to compute the required allocation size.
	 */
	UINT8 storage[];
} VIRTQ_SPLIT;

static __inline size_t VirtqSplitStateSize(UINT16 qsz)
{
	size_t sz = sizeof(VIRTQ_SPLIT);
	sz += sizeof(void *) * (size_t)qsz;
	sz = VIRTIO_ALIGN_UP(sz, (UINT32)sizeof(UINT16));
	sz += sizeof(UINT16) * (size_t)qsz;
	return sz;
}

static __inline BOOLEAN VirtqNeedEvent(UINT16 event, UINT16 new_idx, UINT16 old_idx)
{
	return (UINT16)(new_idx - event - 1) < (UINT16)(new_idx - old_idx);
}

size_t VirtqSplitRingMemSize(UINT16 qsz, UINT32 align, BOOLEAN event_idx);

NTSTATUS VirtqSplitInit(VIRTQ_SPLIT *vq, UINT16 qsz, BOOLEAN event_idx, BOOLEAN indirect, void *ring_va,
			UINT64 ring_pa, UINT32 ring_align, void *indirect_pool_va, UINT64 indirect_pool_pa,
			UINT16 indirect_table_count, UINT16 indirect_max_desc);

VOID VirtqSplitReset(VIRTQ_SPLIT *vq);

NTSTATUS VirtqSplitAddBuffer(VIRTQ_SPLIT *vq, const VIRTQ_SG *sg, UINT16 sg_count, void *cookie,
			     UINT16 *head_out);

VOID VirtqSplitPublish(VIRTQ_SPLIT *vq, UINT16 head);

BOOLEAN VirtqSplitKickPrepare(VIRTQ_SPLIT *vq);
VOID VirtqSplitKickCommit(VIRTQ_SPLIT *vq);

BOOLEAN VirtqSplitHasUsed(const VIRTQ_SPLIT *vq);
NTSTATUS VirtqSplitGetUsed(VIRTQ_SPLIT *vq, void **cookie_out, UINT32 *len_out);

VOID VirtqSplitDisableInterrupts(VIRTQ_SPLIT *vq);
BOOLEAN VirtqSplitEnableInterrupts(VIRTQ_SPLIT *vq);

#endif /* VIRTQUEUE_SPLIT_H_ */
