#include "virtqueue_split.h"

static __inline size_t VirtqSplitAvailSize(UINT16 qsz, BOOLEAN event_idx)
{
	size_t sz = sizeof(UINT16) * 2; /* flags + idx */
	sz += sizeof(UINT16) * (size_t)qsz; /* ring[] */
	if (event_idx) {
		sz += sizeof(UINT16); /* used_event */
	}
	return sz;
}

static __inline size_t VirtqSplitUsedSize(UINT16 qsz, BOOLEAN event_idx)
{
	size_t sz = sizeof(UINT16) * 2; /* flags + idx */
	sz += sizeof(VIRTQ_USED_ELEM) * (size_t)qsz; /* ring[] */
	if (event_idx) {
		sz += sizeof(UINT16); /* avail_event */
	}
	return sz;
}

size_t VirtqSplitRingMemSize(UINT16 qsz, UINT32 align, BOOLEAN event_idx)
{
	size_t desc_sz;
	size_t avail_sz;
	size_t used_off;
	size_t used_sz;

	if (qsz == 0 || align == 0) {
		return 0;
	}
	if ((align & (align - 1)) != 0) {
		return 0;
	}

	desc_sz = sizeof(VIRTQ_DESC) * (size_t)qsz;
	avail_sz = VirtqSplitAvailSize(qsz, event_idx);
	used_off = VIRTIO_ALIGN_UP(desc_sz + avail_sz, align);
	used_sz = VirtqSplitUsedSize(qsz, event_idx);

	return used_off + used_sz;
}

static __inline void VirtqSplitInitStorage(VIRTQ_SPLIT *vq)
{
	UINT8 *base;
	size_t cookie_bytes = sizeof(void *) * (size_t)vq->qsz;
	size_t head_indirect_off = VIRTIO_ALIGN_UP(cookie_bytes, (UINT32)sizeof(UINT16));

	base = (UINT8 *)vq + offsetof(VIRTQ_SPLIT, storage);
	vq->cookies = (void **)base;
	vq->head_indirect = (UINT16 *)(base + head_indirect_off);
}

static __inline UINT16 VirtqSplitAllocDesc(VIRTQ_SPLIT *vq)
{
	UINT16 head;

	if (vq->num_free == 0 || vq->free_head == VIRTQ_SPLIT_NO_DESC) {
		return VIRTQ_SPLIT_NO_DESC;
	}

	head = vq->free_head;
	vq->free_head = vq->desc[head].next;
	vq->num_free--;
	return head;
}

static __inline VOID VirtqSplitFreeDesc(VIRTQ_SPLIT *vq, UINT16 desc_idx)
{
	vq->desc[desc_idx].next = vq->free_head;
	vq->free_head = desc_idx;
	vq->num_free++;
}

static __inline VIRTQ_DESC *VirtqSplitIndirectTable(VIRTQ_SPLIT *vq, UINT16 table_idx)
{
	return (VIRTQ_DESC *)((UINT8 *)vq->indirect_pool_va + (size_t)table_idx * vq->indirect_table_stride);
}

static __inline UINT64 VirtqSplitIndirectTablePa(const VIRTQ_SPLIT *vq, UINT16 table_idx)
{
	return vq->indirect_pool_pa + (UINT64)((size_t)table_idx * vq->indirect_table_stride);
}

static __inline UINT16 VirtqSplitAllocIndirectTable(VIRTQ_SPLIT *vq)
{
	UINT16 table_idx;
	VIRTQ_DESC *table;

	if (!vq->indirect || vq->indirect_num_free == 0 || vq->indirect_free_head == VIRTQ_SPLIT_NO_DESC) {
		return VIRTQ_SPLIT_NO_DESC;
	}

	table_idx = vq->indirect_free_head;
	table = VirtqSplitIndirectTable(vq, table_idx);
	vq->indirect_free_head = table[0].next;
	vq->indirect_num_free--;

	return table_idx;
}

static __inline VOID VirtqSplitFreeIndirectTable(VIRTQ_SPLIT *vq, UINT16 table_idx)
{
	VIRTQ_DESC *table;

	table = VirtqSplitIndirectTable(vq, table_idx);
	table[0].next = vq->indirect_free_head;
	vq->indirect_free_head = table_idx;
	vq->indirect_num_free++;
}

static VOID VirtqSplitFreeChain(VIRTQ_SPLIT *vq, UINT16 head)
{
	UINT16 idx = head;
	UINT16 safety = 0;

	while (idx != VIRTQ_SPLIT_NO_DESC && safety++ < vq->qsz) {
		UINT16 flags = vq->desc[idx].flags;
		UINT16 next = (flags & VIRTQ_DESC_F_NEXT) ? vq->desc[idx].next : VIRTQ_SPLIT_NO_DESC;

		VirtqSplitFreeDesc(vq, idx);

		if ((flags & VIRTQ_DESC_F_NEXT) == 0) {
			break;
		}
		idx = next;
	}
}

NTSTATUS VirtqSplitInit(VIRTQ_SPLIT *vq, UINT16 qsz, BOOLEAN event_idx, BOOLEAN indirect, void *ring_va,
			UINT64 ring_pa, UINT32 ring_align, void *indirect_pool_va, UINT64 indirect_pool_pa,
			UINT16 indirect_table_count, UINT16 indirect_max_desc)
{
	size_t desc_sz;
	size_t avail_sz;
	size_t used_off;

	if (vq == NULL || ring_va == NULL || qsz == 0 || ring_align == 0) {
		return STATUS_INVALID_PARAMETER;
	}
	if ((ring_align & (ring_align - 1)) != 0) {
		/* VIRTIO_ALIGN_UP requires a power-of-two alignment. */
		return STATUS_INVALID_PARAMETER;
	}

	VirtioZeroMemory(vq, sizeof(*vq));

	vq->qsz = qsz;
	vq->event_idx = event_idx ? TRUE : FALSE;
	vq->indirect = indirect ? TRUE : FALSE;
	vq->ring_align = ring_align;

	vq->ring_va = ring_va;
	vq->ring_pa = ring_pa;

	desc_sz = sizeof(VIRTQ_DESC) * (size_t)qsz;
	avail_sz = VirtqSplitAvailSize(qsz, vq->event_idx);
	used_off = VIRTIO_ALIGN_UP(desc_sz + avail_sz, ring_align);

	vq->desc = (VIRTQ_DESC *)ring_va;
	vq->avail = (VIRTQ_AVAIL *)((UINT8 *)ring_va + desc_sz);
	vq->used = (VIRTQ_USED *)((UINT8 *)ring_va + used_off);

	vq->desc_pa = ring_pa;
	vq->avail_pa = ring_pa + (UINT64)desc_sz;
	vq->used_pa = ring_pa + (UINT64)used_off;

	VirtqSplitInitStorage(vq);

	/* Indirect table pool is optional even if the feature is negotiated. */
	if (vq->indirect && indirect_pool_va != NULL && indirect_pool_pa != 0 && indirect_table_count != 0 &&
	    indirect_max_desc != 0) {
		vq->indirect_pool_va = (VIRTQ_DESC *)indirect_pool_va;
		vq->indirect_pool_pa = indirect_pool_pa;
		vq->indirect_table_count = indirect_table_count;
		vq->indirect_max_desc = indirect_max_desc;
		vq->indirect_table_stride = sizeof(VIRTQ_DESC) * (UINT32)indirect_max_desc;

		/*
		 * Default policy: prefer indirect above 8 SG entries even if
		 * enough direct descriptors are available, to keep the ring free
		 * for other requests. This value can be tuned by callers by
		 * editing vq->indirect_threshold after init.
		 */
		vq->indirect_threshold = 8;
	} else {
		vq->indirect_pool_va = NULL;
		vq->indirect_pool_pa = 0;
		vq->indirect_table_count = 0;
		vq->indirect_max_desc = 0;
		vq->indirect_table_stride = 0;
		vq->indirect_threshold = 0;
	}

	VirtqSplitReset(vq);

	return STATUS_SUCCESS;
}

VOID VirtqSplitReset(VIRTQ_SPLIT *vq)
{
	UINT16 i;

	if (vq == NULL || vq->qsz == 0) {
		return;
	}

	vq->avail_idx = 0;
	vq->last_used_idx = 0;
	vq->num_added = 0;

	vq->num_free = vq->qsz;
	vq->free_head = (vq->qsz == 0) ? VIRTQ_SPLIT_NO_DESC : 0;

	for (i = 0; i < vq->qsz; i++) {
		vq->desc[i].addr = 0;
		vq->desc[i].len = 0;
		vq->desc[i].flags = 0;
		vq->desc[i].next = (i + 1 < vq->qsz) ? (UINT16)(i + 1) : VIRTQ_SPLIT_NO_DESC;
	}

	for (i = 0; i < vq->qsz; i++) {
		vq->cookies[i] = NULL;
		vq->head_indirect[i] = VIRTQ_SPLIT_NO_DESC;
	}

	/* Reset ring indices/flags visible to the device. */
	VirtioWriteU16((volatile UINT16 *)&vq->avail->flags, 0);
	VirtioWriteU16((volatile UINT16 *)&vq->avail->idx, 0);
	VirtioWriteU16((volatile UINT16 *)&vq->used->flags, 0);
	VirtioWriteU16((volatile UINT16 *)&vq->used->idx, 0);

	if (vq->event_idx) {
		VirtioWriteU16(VirtqAvailUsedEvent(vq->avail, vq->qsz), 0);
		/*
		 * used->avail_event is device-written; clearing it is harmless
		 * and simplifies unit tests.
		 */
		VirtioWriteU16(VirtqUsedAvailEvent(vq->used, vq->qsz), 0);
	}

	if (vq->indirect_pool_va != NULL && vq->indirect_table_count != 0) {
		vq->indirect_free_head = 0;
		vq->indirect_num_free = vq->indirect_table_count;

		for (i = 0; i < vq->indirect_table_count; i++) {
			VIRTQ_DESC *table = VirtqSplitIndirectTable(vq, i);
			table[0].next =
				(i + 1 < vq->indirect_table_count) ? (UINT16)(i + 1) : VIRTQ_SPLIT_NO_DESC;
		}
	} else {
		vq->indirect_free_head = VIRTQ_SPLIT_NO_DESC;
		vq->indirect_num_free = 0;
	}
}

NTSTATUS VirtqSplitAddBuffer(VIRTQ_SPLIT *vq, const VIRTQ_SG *sg, UINT16 sg_count, void *cookie,
			     UINT16 *head_out)
{
	UINT16 head;
	UINT16 i;
	BOOLEAN want_indirect;

	if (vq == NULL || sg == NULL || sg_count == 0 || head_out == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	want_indirect = FALSE;
	if (vq->indirect_pool_va != NULL && vq->indirect_num_free != 0 && sg_count <= vq->indirect_max_desc) {
		if (sg_count > vq->num_free || sg_count > vq->indirect_threshold) {
			want_indirect = TRUE;
		}
	}

	if (want_indirect) {
		UINT16 table_idx;
		VIRTQ_DESC *table;
		VIRTQ_DESC *d;

		/* Indirect consumes 1 ring descriptor. */
		if (vq->num_free < 1) {
			return STATUS_INSUFFICIENT_RESOURCES;
		}

		table_idx = VirtqSplitAllocIndirectTable(vq);
		if (table_idx == VIRTQ_SPLIT_NO_DESC) {
			/* Pool exhausted; fall back to direct if possible. */
			want_indirect = FALSE;
		} else {
			head = VirtqSplitAllocDesc(vq);
			if (head == VIRTQ_SPLIT_NO_DESC) {
				VirtqSplitFreeIndirectTable(vq, table_idx);
				return STATUS_INSUFFICIENT_RESOURCES;
			}

			table = VirtqSplitIndirectTable(vq, table_idx);
			for (i = 0; i < sg_count; i++) {
				UINT16 flags = sg[i].write ? VIRTQ_DESC_F_WRITE : 0;
				if (i + 1 < sg_count) {
					flags |= VIRTQ_DESC_F_NEXT;
				}
				table[i].addr = sg[i].addr;
				table[i].len = sg[i].len;
				table[i].flags = flags;
				table[i].next = (i + 1 < sg_count) ? (UINT16)(i + 1) : 0;
			}

			d = &vq->desc[head];
			d->addr = VirtqSplitIndirectTablePa(vq, table_idx);
			d->len = (UINT32)(sizeof(VIRTQ_DESC) * (UINT32)sg_count);
			d->flags = VIRTQ_DESC_F_INDIRECT;
			d->next = 0;

			vq->cookies[head] = cookie;
			vq->head_indirect[head] = table_idx;

			*head_out = head;
			return STATUS_SUCCESS;
		}
	}

	/* Direct chain */
	if (vq->num_free < sg_count) {
		return STATUS_INSUFFICIENT_RESOURCES;
	}

	head = VirtqSplitAllocDesc(vq);
	if (head == VIRTQ_SPLIT_NO_DESC) {
		return STATUS_INSUFFICIENT_RESOURCES;
	}

	{
		UINT16 idx = head;

		for (i = 0; i < sg_count; i++) {
			VIRTQ_DESC *d = &vq->desc[idx];
			UINT16 flags = sg[i].write ? VIRTQ_DESC_F_WRITE : 0;

			d->addr = sg[i].addr;
			d->len = sg[i].len;

			if (i + 1 < sg_count) {
				UINT16 next = VirtqSplitAllocDesc(vq);
				flags |= VIRTQ_DESC_F_NEXT;
				d->flags = flags;
				d->next = next;
				idx = next;
			} else {
				d->flags = flags;
				d->next = 0;
			}
		}
	}

	vq->cookies[head] = cookie;
	vq->head_indirect[head] = VIRTQ_SPLIT_NO_DESC;

	*head_out = head;
	return STATUS_SUCCESS;
}

VOID VirtqSplitPublish(VIRTQ_SPLIT *vq, UINT16 head)
{
	UINT16 slot;
	UINT16 new_idx;

	if (vq == NULL || vq->qsz == 0) {
		return;
	}

	slot = (UINT16)(vq->avail_idx % vq->qsz);
	VirtioWriteU16((volatile UINT16 *)&VirtqAvailRing(vq->avail)[slot], head);

	new_idx = (UINT16)(vq->avail_idx + 1);
	vq->avail_idx = new_idx;
	vq->num_added++;

	/* Make descriptor writes visible before updating avail->idx. */
	VIRTIO_WMB();
	VirtioWriteU16((volatile UINT16 *)&vq->avail->idx, new_idx);
}

BOOLEAN VirtqSplitKickPrepare(VIRTQ_SPLIT *vq)
{
	UINT16 new_avail;
	UINT16 old_avail;

	if (vq == NULL || vq->num_added == 0) {
		return FALSE;
	}

	new_avail = vq->avail_idx;
	old_avail = (UINT16)(new_avail - vq->num_added);

	if (vq->event_idx) {
		UINT16 event = VirtioReadU16(VirtqUsedAvailEvent(vq->used, vq->qsz));
		return VirtqNeedEvent(event, new_avail, old_avail);
	}

	return (VirtioReadU16((volatile UINT16 *)&vq->used->flags) & VIRTQ_USED_F_NO_NOTIFY) == 0;
}

VOID VirtqSplitKickCommit(VIRTQ_SPLIT *vq)
{
	if (vq == NULL) {
		return;
	}
	vq->num_added = 0;
}

BOOLEAN VirtqSplitHasUsed(const VIRTQ_SPLIT *vq)
{
	UINT16 used_idx;

	if (vq == NULL) {
		return FALSE;
	}

	used_idx = VirtioReadU16((volatile UINT16 *)&vq->used->idx);
	return used_idx != vq->last_used_idx;
}

NTSTATUS VirtqSplitGetUsed(VIRTQ_SPLIT *vq, void **cookie_out, UINT32 *len_out)
{
	UINT16 used_idx;
	UINT16 slot;
	UINT32 id;
	UINT32 len;
	UINT16 head;
	UINT16 table_idx;

	if (vq == NULL || cookie_out == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	used_idx = VirtioReadU16((volatile UINT16 *)&vq->used->idx);
	if (used_idx == vq->last_used_idx) {
		return STATUS_NOT_FOUND;
	}

	/*
	 * Ensure the used ring entry (and device-written buffers) are visible
	 * after observing used->idx advancing.
	 */
	VIRTIO_RMB();

	slot = (UINT16)(vq->last_used_idx % vq->qsz);
	{
		VIRTQ_USED_ELEM *used_ring = VirtqUsedRing(vq->used);
		id = VirtioReadU32((volatile UINT32 *)&used_ring[slot].id);
		len = VirtioReadU32((volatile UINT32 *)&used_ring[slot].len);
	}

	if (id >= vq->qsz) {
		return STATUS_INVALID_PARAMETER;
	}

	head = (UINT16)id;
	*cookie_out = vq->cookies[head];
	if (len_out != NULL) {
		*len_out = len;
	}

	vq->cookies[head] = NULL;

	table_idx = vq->head_indirect[head];
	if (table_idx != VIRTQ_SPLIT_NO_DESC) {
		vq->head_indirect[head] = VIRTQ_SPLIT_NO_DESC;
		if (vq->indirect_pool_va != NULL && table_idx < vq->indirect_table_count) {
			VirtqSplitFreeIndirectTable(vq, table_idx);
		}
	}

	VirtqSplitFreeChain(vq, head);

	vq->last_used_idx = (UINT16)(vq->last_used_idx + 1);

	return STATUS_SUCCESS;
}

VOID VirtqSplitDisableInterrupts(VIRTQ_SPLIT *vq)
{
	if (vq == NULL) {
		return;
	}

	if (vq->event_idx) {
		VirtioWriteU16(VirtqAvailUsedEvent(vq->avail, vq->qsz), (UINT16)(vq->last_used_idx - 1));
	} else {
		UINT16 flags = VirtioReadU16((volatile UINT16 *)&vq->avail->flags);
		flags |= VIRTQ_AVAIL_F_NO_INTERRUPT;
		VirtioWriteU16((volatile UINT16 *)&vq->avail->flags, flags);
	}
}

BOOLEAN VirtqSplitEnableInterrupts(VIRTQ_SPLIT *vq)
{
	UINT16 used_idx;

	if (vq == NULL) {
		return FALSE;
	}

	if (vq->event_idx) {
		VirtioWriteU16(VirtqAvailUsedEvent(vq->avail, vq->qsz), vq->last_used_idx);
	} else {
		UINT16 flags = VirtioReadU16((volatile UINT16 *)&vq->avail->flags);
		flags &= (UINT16)~VIRTQ_AVAIL_F_NO_INTERRUPT;
		VirtioWriteU16((volatile UINT16 *)&vq->avail->flags, flags);
	}

	/* Avoid missing an interrupt between enabling and checking used->idx. */
	VIRTIO_MB();
	used_idx = VirtioReadU16((volatile UINT16 *)&vq->used->idx);
	return used_idx == vq->last_used_idx;
}

#ifdef VIRTQ_DEBUG
#if !VIRTIO_OSDEP_KERNEL_MODE
#include <stdarg.h>
#include <stdio.h>

static void VirtqSplitDumpLog(void (*logfn)(const char *line, void *ctx), void *ctx, const char *fmt, ...)
{
	char buf[256];
	va_list ap;

	va_start(ap, fmt);
	vsnprintf(buf, sizeof(buf), fmt, ap);
	va_end(ap);

	logfn(buf, ctx);
}

VOID VirtqSplitDump(const VIRTQ_SPLIT *vq, void (*logfn)(const char *line, void *ctx), void *ctx)
{
	if (vq == NULL || logfn == NULL) {
		return;
	}

	VirtqSplitDumpLog(logfn, ctx,
			  "VIRTQ_SPLIT qsz=%u event_idx=%u indirect=%u ring_align=%u", (unsigned)vq->qsz,
			  (unsigned)(vq->event_idx != FALSE), (unsigned)(vq->indirect != FALSE),
			  (unsigned)vq->ring_align);
	VirtqSplitDumpLog(logfn, ctx,
			  "  avail_idx=%u last_used_idx=%u num_added=%u", (unsigned)vq->avail_idx,
			  (unsigned)vq->last_used_idx, (unsigned)vq->num_added);
	VirtqSplitDumpLog(logfn, ctx,
			  "  free_head=%u num_free=%u", (unsigned)vq->free_head, (unsigned)vq->num_free);

	VirtqSplitDumpLog(logfn, ctx,
			  "  avail->idx=%u avail->flags=0x%04x", (unsigned)VirtioReadU16((volatile UINT16 *)&vq->avail->idx),
			  (unsigned)VirtioReadU16((volatile UINT16 *)&vq->avail->flags));
	VirtqSplitDumpLog(logfn, ctx,
			  "  used->idx=%u used->flags=0x%04x", (unsigned)VirtioReadU16((volatile UINT16 *)&vq->used->idx),
			  (unsigned)VirtioReadU16((volatile UINT16 *)&vq->used->flags));

	if (vq->event_idx) {
		VirtqSplitDumpLog(logfn, ctx,
				  "  used_event=%u avail_event=%u", (unsigned)VirtioReadU16(VirtqAvailUsedEvent(vq->avail, vq->qsz)),
				  (unsigned)VirtioReadU16(VirtqUsedAvailEvent(vq->used, vq->qsz)));
	}

	if (vq->indirect_pool_va != NULL) {
		VirtqSplitDumpLog(logfn, ctx,
				  "  indirect_table_count=%u indirect_num_free=%u indirect_free_head=%u indirect_threshold=%u max_desc=%u",
				  (unsigned)vq->indirect_table_count, (unsigned)vq->indirect_num_free,
				  (unsigned)vq->indirect_free_head, (unsigned)vq->indirect_threshold,
				  (unsigned)vq->indirect_max_desc);
	}
}
#else
VOID VirtqSplitDump(const VIRTQ_SPLIT *vq, void (*logfn)(const char *line, void *ctx), void *ctx)
{
	(void)vq;
	(void)logfn;
	(void)ctx;
}
#endif
#endif
