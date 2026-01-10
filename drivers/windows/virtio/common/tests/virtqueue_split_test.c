#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../virtqueue_split.h"

#define ASSERT_TRUE(cond)                                                                                            \
	do {                                                                                                         \
		if (!(cond)) {                                                                                        \
			fprintf(stderr, "ASSERT_TRUE failed at %s:%d: %s\n", __FILE__, __LINE__, #cond);              \
			abort();                                                                                     \
		}                                                                                                    \
	} while (0)

#define ASSERT_EQ_U16(a, b) ASSERT_TRUE((UINT16)(a) == (UINT16)(b))
#define ASSERT_EQ_U32(a, b) ASSERT_TRUE((UINT32)(a) == (UINT32)(b))
#define ASSERT_EQ_U64(a, b) ASSERT_TRUE((UINT64)(a) == (UINT64)(b))

static void AssertRingFreeListIntact(const VIRTQ_SPLIT *vq)
{
	UINT8 *seen;
	UINT16 idx;
	UINT16 count = 0;

	seen = (UINT8 *)calloc(vq->qsz, 1);
	ASSERT_TRUE(seen != NULL);

	idx = vq->free_head;
	while (idx != VIRTQ_SPLIT_NO_DESC) {
		ASSERT_TRUE(idx < vq->qsz);
		ASSERT_TRUE(seen[idx] == 0);
		seen[idx] = 1;
		count++;
		idx = vq->desc[idx].next;
	}

	ASSERT_EQ_U16(count, vq->num_free);
	free(seen);
}

static void AssertIndirectFreeListIntact(const VIRTQ_SPLIT *vq)
{
	UINT8 *seen;
	UINT16 idx;
	UINT16 count = 0;

	if (vq->indirect_pool_va == NULL || vq->indirect_table_count == 0) {
		return;
	}

	seen = (UINT8 *)calloc(vq->indirect_table_count, 1);
	ASSERT_TRUE(seen != NULL);

	idx = vq->indirect_free_head;
	while (idx != VIRTQ_SPLIT_NO_DESC) {
		VIRTQ_DESC *table;
		ASSERT_TRUE(idx < vq->indirect_table_count);
		ASSERT_TRUE(seen[idx] == 0);
		seen[idx] = 1;
		count++;
		table = (VIRTQ_DESC *)((UINT8 *)vq->indirect_pool_va + (size_t)idx * vq->indirect_table_stride);
		idx = table[0].next;
	}

	ASSERT_EQ_U16(count, vq->indirect_num_free);
	free(seen);
}

static UINT16 DeviceConsumeAvailOne(VIRTQ_SPLIT *vq, UINT16 *dev_avail_idx, UINT16 *dev_used_idx, const VIRTQ_SG *exp_sg,
				   UINT16 exp_sg_count, UINT32 used_len)
{
	UINT16 avail_idx;
	UINT16 head;
	UINT16 used_slot;

	avail_idx = VirtioReadU16((volatile UINT16 *)&vq->avail->idx);
	ASSERT_TRUE(*dev_avail_idx != avail_idx);

	head = VirtioReadU16((volatile UINT16 *)&VirtqAvailRing(vq->avail)[(UINT16)(*dev_avail_idx % vq->qsz)]);

	/* Validate descriptor chain as the device would. */
	{
		VIRTQ_DESC *d = &vq->desc[head];

		if ((d->flags & VIRTQ_DESC_F_INDIRECT) != 0) {
			VIRTQ_DESC *table = (VIRTQ_DESC *)(uintptr_t)d->addr;
			UINT16 i;

			ASSERT_EQ_U32(d->len, (UINT32)(sizeof(VIRTQ_DESC) * (UINT32)exp_sg_count));
			ASSERT_TRUE((d->flags & VIRTQ_DESC_F_NEXT) == 0);

			for (i = 0; i < exp_sg_count; i++) {
				UINT16 flags = table[i].flags;
				ASSERT_EQ_U64(table[i].addr, exp_sg[i].addr);
				ASSERT_EQ_U32(table[i].len, exp_sg[i].len);
				ASSERT_TRUE(((flags & VIRTQ_DESC_F_WRITE) != 0) == (exp_sg[i].write != FALSE));
				if (i + 1 < exp_sg_count) {
					ASSERT_TRUE((flags & VIRTQ_DESC_F_NEXT) != 0);
					ASSERT_EQ_U16(table[i].next, (UINT16)(i + 1));
				} else {
					ASSERT_TRUE((flags & VIRTQ_DESC_F_NEXT) == 0);
				}
			}
		} else {
			UINT16 idx = head;
			UINT16 i;

			for (i = 0; i < exp_sg_count; i++) {
				UINT16 flags;
				d = &vq->desc[idx];
				flags = d->flags;

				ASSERT_EQ_U64(d->addr, exp_sg[i].addr);
				ASSERT_EQ_U32(d->len, exp_sg[i].len);
				ASSERT_TRUE(((flags & VIRTQ_DESC_F_WRITE) != 0) == (exp_sg[i].write != FALSE));

				if (i + 1 < exp_sg_count) {
					ASSERT_TRUE((flags & VIRTQ_DESC_F_NEXT) != 0);
					idx = d->next;
				} else {
					ASSERT_TRUE((flags & VIRTQ_DESC_F_NEXT) == 0);
				}
				ASSERT_TRUE((flags & VIRTQ_DESC_F_INDIRECT) == 0);
			}
		}
	}

	used_slot = (UINT16)(*dev_used_idx % vq->qsz);
	{
		VIRTQ_USED_ELEM *used_ring = VirtqUsedRing(vq->used);
		VirtioWriteU32((volatile UINT32 *)&used_ring[used_slot].id, head);
		VirtioWriteU32((volatile UINT32 *)&used_ring[used_slot].len, used_len);
	}
	(*dev_used_idx)++;
	(*dev_avail_idx)++;

	VIRTIO_WMB();
	VirtioWriteU16((volatile UINT16 *)&vq->used->idx, *dev_used_idx);

	return head;
}

static void TestDirectChainAddFree(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);

	UINT8 buf1[16], buf2[32], buf3[64];
	VIRTQ_SG sg[3];
	UINT16 head;
	UINT16 dev_avail_idx = 0;
	UINT16 dev_used_idx = 0;
	void *cookie_out = NULL;
	UINT32 len_out = 0;

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	sg[0].addr = (UINT64)(uintptr_t)buf1;
	sg[0].len = sizeof(buf1);
	sg[0].write = FALSE;

	sg[1].addr = (UINT64)(uintptr_t)buf2;
	sg[1].len = sizeof(buf2);
	sg[1].write = TRUE;

	sg[2].addr = (UINT64)(uintptr_t)buf3;
	sg[2].len = sizeof(buf3);
	sg[2].write = TRUE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, FALSE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 3, (void *)0x1234, &head)));
	VirtqSplitPublish(vq, head);

	DeviceConsumeAvailOne(vq, &dev_avail_idx, &dev_used_idx, sg, 3, 0xBEEF);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0x1234);
	ASSERT_EQ_U32(len_out, 0xBEEF);

	ASSERT_EQ_U16(vq->num_free, qsz);
	AssertRingFreeListIntact(vq);

	free(ring);
	free(vq);
}

static void TestWraparound(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);

	UINT8 buf_a[8], buf_b[8];
	VIRTQ_SG sg_a[1], sg_b[1];
	UINT16 head_a, head_b;
	UINT16 dev_avail_idx, dev_used_idx;
	void *cookie_out = NULL;
	UINT32 len_out = 0;

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	sg_a[0].addr = (UINT64)(uintptr_t)buf_a;
	sg_a[0].len = sizeof(buf_a);
	sg_a[0].write = TRUE;

	sg_b[0].addr = (UINT64)(uintptr_t)buf_b;
	sg_b[0].len = sizeof(buf_b);
	sg_b[0].write = TRUE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, FALSE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	/* Force indices near 0xFFFF to exercise wrap-safe arithmetic. */
	vq->avail_idx = 0xFFFE;
	vq->last_used_idx = 0xFFFE;
	VirtioWriteU16((volatile UINT16 *)&vq->avail->idx, 0xFFFE);
	VirtioWriteU16((volatile UINT16 *)&vq->used->idx, 0xFFFE);

	dev_avail_idx = 0xFFFE;
	dev_used_idx = 0xFFFE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg_a, 1, (void *)0xAAAA, &head_a)));
	VirtqSplitPublish(vq, head_a); /* avail_idx -> 0xFFFF */

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg_b, 1, (void *)0xBBBB, &head_b)));
	VirtqSplitPublish(vq, head_b); /* avail_idx -> 0x0000 */

	ASSERT_EQ_U16(vq->avail_idx, 0x0000);

	DeviceConsumeAvailOne(vq, &dev_avail_idx, &dev_used_idx, sg_a, 1, 1);
	DeviceConsumeAvailOne(vq, &dev_avail_idx, &dev_used_idx, sg_b, 1, 2);
	ASSERT_EQ_U16(dev_used_idx, 0x0000);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0xAAAA);
	ASSERT_EQ_U32(len_out, 1);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0xBBBB);
	ASSERT_EQ_U32(len_out, 2);

	ASSERT_EQ_U16(vq->last_used_idx, 0x0000);
	ASSERT_EQ_U16(vq->num_free, qsz);
	AssertRingFreeListIntact(vq);

	free(ring);
	free(vq);
}

static void TestOutOfOrderCompletion(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);

	UINT8 buf_a[8], buf_b[8];
	VIRTQ_SG sg_a[1], sg_b[1];
	UINT16 head_a, head_b;
	void *cookie_out = NULL;
	UINT32 len_out = 0;
	UINT16 dev_avail_idx = 0;
	UINT16 dev_used_idx = 0;

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	sg_a[0].addr = (UINT64)(uintptr_t)buf_a;
	sg_a[0].len = sizeof(buf_a);
	sg_a[0].write = TRUE;

	sg_b[0].addr = (UINT64)(uintptr_t)buf_b;
	sg_b[0].len = sizeof(buf_b);
	sg_b[0].write = TRUE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, FALSE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg_a, 1, (void *)0xAAAA, &head_a)));
	VirtqSplitPublish(vq, head_a);
	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg_b, 1, (void *)0xBBBB, &head_b)));
	VirtqSplitPublish(vq, head_b);

	/* Device consumes both avail entries. */
	ASSERT_EQ_U16(VirtioReadU16((volatile UINT16 *)&vq->avail->idx), 2);
	ASSERT_EQ_U16(dev_avail_idx, 0);
	head_a = VirtioReadU16((volatile UINT16 *)&VirtqAvailRing(vq->avail)[(UINT16)(dev_avail_idx++ % qsz)]);
	head_b = VirtioReadU16((volatile UINT16 *)&VirtqAvailRing(vq->avail)[(UINT16)(dev_avail_idx++ % qsz)]);

	/* Device completes out-of-order: B then A. */
	{
		VIRTQ_USED_ELEM *used_ring = VirtqUsedRing(vq->used);
		VirtioWriteU32((volatile UINT32 *)&used_ring[(UINT16)(dev_used_idx % qsz)].id, head_b);
		VirtioWriteU32((volatile UINT32 *)&used_ring[(UINT16)(dev_used_idx % qsz)].len, 2);
		dev_used_idx++;

		VirtioWriteU32((volatile UINT32 *)&used_ring[(UINT16)(dev_used_idx % qsz)].id, head_a);
		VirtioWriteU32((volatile UINT32 *)&used_ring[(UINT16)(dev_used_idx % qsz)].len, 1);
		dev_used_idx++;
	}

	VIRTIO_WMB();
	VirtioWriteU16((volatile UINT16 *)&vq->used->idx, dev_used_idx);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0xBBBB);
	ASSERT_EQ_U32(len_out, 2);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0xAAAA);
	ASSERT_EQ_U32(len_out, 1);

	ASSERT_EQ_U16(vq->num_free, qsz);
	AssertRingFreeListIntact(vq);

	free(ring);
	free(vq);
}

static void TestNeedEventBoundaryCases(void)
{
	ASSERT_TRUE(VirtqNeedEvent(0, 1, 0) != FALSE);
	ASSERT_TRUE(VirtqNeedEvent(1, 1, 0) == FALSE);
	ASSERT_TRUE(VirtqNeedEvent(0, 0, 0) == FALSE);

	/* Wraparound interval: old=0xFFFE, new=0x0001 (delta=3). */
	ASSERT_TRUE(VirtqNeedEvent(0xFFFF, 0x0001, 0xFFFE) != FALSE);
	ASSERT_TRUE(VirtqNeedEvent(0x0000, 0x0001, 0xFFFE) != FALSE);
	ASSERT_TRUE(VirtqNeedEvent(0x0001, 0x0001, 0xFFFE) == FALSE);
}

static void TestRingLayoutEventIdx(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t desc_sz = sizeof(VIRTQ_DESC) * (size_t)qsz;
	size_t avail_sz = 4 + (2 * (size_t)qsz) + 2; /* flags+idx + ring + used_event */
	size_t used_off = VIRTIO_ALIGN_UP(desc_sz + avail_sz, align);
	size_t used_sz = 4 + (sizeof(VIRTQ_USED_ELEM) * (size_t)qsz) + 2; /* flags+idx + ring + avail_event */
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, TRUE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, VirtqSplitStateSize(qsz));
	void *ring = calloc(1, ring_bytes);

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	ASSERT_EQ_U64((UINT64)ring_bytes, (UINT64)(used_off + used_sz));
	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, TRUE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	ASSERT_TRUE((void *)vq->desc == ring);
	ASSERT_TRUE((void *)vq->avail == (void *)((UINT8 *)ring + desc_sz));
	ASSERT_TRUE((void *)vq->used == (void *)((UINT8 *)ring + used_off));
	ASSERT_TRUE((((uintptr_t)vq->used) & (align - 1)) == 0);

	ASSERT_TRUE((void *)VirtqAvailUsedEvent(vq->avail, qsz) == (void *)((UINT8 *)vq->avail + 4 + (2 * (size_t)qsz)));
	ASSERT_TRUE((void *)VirtqUsedAvailEvent(vq->used, qsz) ==
		    (void *)((UINT8 *)vq->used + 4 + (sizeof(VIRTQ_USED_ELEM) * (size_t)qsz)));

	free(ring);
	free(vq);
}

static void TestInitRejectsMisalignedRing(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	UINT8 *ring_raw = (UINT8 *)calloc(1, ring_bytes + 1);

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring_raw != NULL);

	/* Deliberately misalign ring VA/PA by 1 byte. */
	ASSERT_EQ_U32(VirtqSplitInit(vq, qsz, FALSE, FALSE, ring_raw + 1, (UINT64)(uintptr_t)(ring_raw + 1), align, NULL, 0, 0, 0),
		      STATUS_INVALID_PARAMETER);

	free(ring_raw);
	free(vq);
}

static void TestEventIdxKickPrepare(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, TRUE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);

	UINT8 buf[16];
	VIRTQ_SG sg[1];
	UINT16 head;

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	sg[0].addr = (UINT64)(uintptr_t)buf;
	sg[0].len = sizeof(buf);
	sg[0].write = TRUE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, TRUE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	/* Device asks for notification at avail index 0 -> first publish should notify. */
	VirtioWriteU16(VirtqUsedAvailEvent(vq->used, vq->qsz), 0);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 1, (void *)0x1, &head)));
	VirtqSplitPublish(vq, head);
	ASSERT_TRUE(VirtqSplitKickPrepare(vq) != FALSE);
	VirtqSplitKickCommit(vq);

	/* Device asks for notification at index 2 -> single publish to 1 should not notify. */
	VirtioWriteU16(VirtqUsedAvailEvent(vq->used, vq->qsz), 2);
	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 1, (void *)0x2, &head)));
	VirtqSplitPublish(vq, head);
	ASSERT_TRUE(VirtqSplitKickPrepare(vq) == FALSE);
	VirtqSplitKickCommit(vq);

	free(ring);
	free(vq);
}

static void TestNoNotifyKickPrepare(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);

	UINT8 buf[16];
	VIRTQ_SG sg[1];
	UINT16 head;

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	sg[0].addr = (UINT64)(uintptr_t)buf;
	sg[0].len = sizeof(buf);
	sg[0].write = TRUE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, FALSE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	/* When NO_NOTIFY is set by the device, the driver should suppress kicks. */
	VirtioWriteU16((volatile UINT16 *)&vq->used->flags, VIRTQ_USED_F_NO_NOTIFY);
	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 1, (void *)0x1, &head)));
	VirtqSplitPublish(vq, head);
	ASSERT_TRUE(VirtqSplitKickPrepare(vq) == FALSE);
	VirtqSplitKickCommit(vq);

	/* When NO_NOTIFY is clear, the driver should kick if it added buffers. */
	VirtioWriteU16((volatile UINT16 *)&vq->used->flags, 0);
	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 1, (void *)0x2, &head)));
	VirtqSplitPublish(vq, head);
	ASSERT_TRUE(VirtqSplitKickPrepare(vq) != FALSE);
	VirtqSplitKickCommit(vq);

	free(ring);
	free(vq);
}

static void TestIndirectPoolExhaustionFallsBackToDirect(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	const UINT16 table_count = 1;
	const UINT16 max_desc = 4;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);
	size_t pool_bytes = (size_t)table_count * max_desc * sizeof(VIRTQ_DESC);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);
	void *pool = calloc(1, pool_bytes);

	UINT8 buf1[8], buf2[8];
	VIRTQ_SG sg[2];
	UINT16 head_indirect;
	UINT16 head_direct;
	UINT16 dev_avail_idx = 0;
	UINT16 dev_used_idx = 0;
	void *cookie_out = NULL;
	UINT32 len_out = 0;

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);
	ASSERT_TRUE(pool != NULL);

	sg[0].addr = (UINT64)(uintptr_t)buf1;
	sg[0].len = sizeof(buf1);
	sg[0].write = FALSE;

	sg[1].addr = (UINT64)(uintptr_t)buf2;
	sg[1].len = sizeof(buf2);
	sg[1].write = TRUE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, FALSE, TRUE, ring, (UINT64)(uintptr_t)ring, align, pool,
					     (UINT64)(uintptr_t)pool, table_count, max_desc)));

	/* Force indirect for sg_count=2 while a table is available. */
	vq->indirect_threshold = 1;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 2, (void *)0x1111, &head_indirect)));
	ASSERT_TRUE((vq->desc[head_indirect].flags & VIRTQ_DESC_F_INDIRECT) != 0);
	ASSERT_EQ_U16(vq->indirect_num_free, 0);
	VirtqSplitPublish(vq, head_indirect);

	/* Pool is now exhausted; next buffer should fall back to direct chaining. */
	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 2, (void *)0x2222, &head_direct)));
	ASSERT_TRUE((vq->desc[head_direct].flags & VIRTQ_DESC_F_INDIRECT) == 0);
	VirtqSplitPublish(vq, head_direct);

	DeviceConsumeAvailOne(vq, &dev_avail_idx, &dev_used_idx, sg, 2, 11);
	DeviceConsumeAvailOne(vq, &dev_avail_idx, &dev_used_idx, sg, 2, 22);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0x1111);
	ASSERT_EQ_U32(len_out, 11);
	ASSERT_EQ_U16(vq->indirect_num_free, 1);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0x2222);
	ASSERT_EQ_U32(len_out, 22);

	ASSERT_EQ_U16(vq->num_free, qsz);
	ASSERT_EQ_U16(vq->indirect_num_free, table_count);
	AssertRingFreeListIntact(vq);
	AssertIndirectFreeListIntact(vq);

	free(pool);
	free(ring);
	free(vq);
}

static void TestInterruptSuppressionNoEventIdx(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, FALSE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	VirtqSplitDisableInterrupts(vq);
	ASSERT_TRUE((VirtioReadU16((volatile UINT16 *)&vq->avail->flags) & VIRTQ_AVAIL_F_NO_INTERRUPT) != 0);

	/* No pending used entries -> safe to sleep. */
	ASSERT_TRUE(VirtqSplitEnableInterrupts(vq) != FALSE);
	ASSERT_TRUE((VirtioReadU16((volatile UINT16 *)&vq->avail->flags) & VIRTQ_AVAIL_F_NO_INTERRUPT) == 0);

	/* Pending used entries -> caller should poll. */
	VirtioWriteU16((volatile UINT16 *)&vq->used->idx, 1);
	ASSERT_TRUE(VirtqSplitEnableInterrupts(vq) == FALSE);

	free(ring);
	free(vq);
}

static void TestInterruptSuppressionEventIdx(void)
{
	const UINT16 qsz = 8;
	const UINT32 align = 16;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, TRUE);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, TRUE, FALSE, ring, (UINT64)(uintptr_t)ring, align, NULL, 0, 0, 0)));

	VirtqSplitDisableInterrupts(vq);
	ASSERT_EQ_U16(VirtioReadU16(VirtqAvailUsedEvent(vq->avail, vq->qsz)), (UINT16)(vq->last_used_idx - 1));

	/* No pending used entries -> safe to sleep. */
	ASSERT_TRUE(VirtqSplitEnableInterrupts(vq) != FALSE);
	ASSERT_EQ_U16(VirtioReadU16(VirtqAvailUsedEvent(vq->avail, vq->qsz)), vq->last_used_idx);

	/* Pending used entries -> caller should poll. */
	VirtioWriteU16((volatile UINT16 *)&vq->used->idx, 1);
	ASSERT_TRUE(VirtqSplitEnableInterrupts(vq) == FALSE);

	free(ring);
	free(vq);
}

static void TestIndirectDescriptors(void)
{
	const UINT16 qsz = 2;
	const UINT32 align = 16;

	const UINT16 table_count = 1;
	const UINT16 max_desc = 8;

	size_t vq_bytes = VirtqSplitStateSize(qsz);
	size_t ring_bytes = VirtqSplitRingMemSize(qsz, align, FALSE);
	size_t pool_bytes = (size_t)table_count * max_desc * sizeof(VIRTQ_DESC);

	VIRTQ_SPLIT *vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	void *ring = calloc(1, ring_bytes);
	void *pool = calloc(1, pool_bytes);

	UINT8 buf1[4], buf2[4], buf3[4];
	VIRTQ_SG sg[3];
	UINT16 head;
	UINT16 dev_avail_idx = 0;
	UINT16 dev_used_idx = 0;
	void *cookie_out = NULL;
	UINT32 len_out = 0;

	ASSERT_TRUE(vq != NULL);
	ASSERT_TRUE(ring != NULL);
	ASSERT_TRUE(pool != NULL);

	sg[0].addr = (UINT64)(uintptr_t)buf1;
	sg[0].len = sizeof(buf1);
	sg[0].write = FALSE;

	sg[1].addr = (UINT64)(uintptr_t)buf2;
	sg[1].len = sizeof(buf2);
	sg[1].write = TRUE;

	sg[2].addr = (UINT64)(uintptr_t)buf3;
	sg[2].len = sizeof(buf3);
	sg[2].write = TRUE;

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(vq, qsz, FALSE, TRUE, ring, (UINT64)(uintptr_t)ring, align, pool,
					     (UINT64)(uintptr_t)pool, table_count, max_desc)));
	ASSERT_EQ_U16(vq->indirect_num_free, table_count);

	/*
	 * With qsz=2 and sg_count=3, direct chaining would require 3 ring
	 * descriptors (impossible). The implementation should pick indirect.
	 */
	ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(vq, sg, 3, (void *)0xCAFE, &head)));
	ASSERT_EQ_U16(vq->num_free, (UINT16)(qsz - 1));
	ASSERT_EQ_U16(vq->indirect_num_free, 0);

	/* Verify the main descriptor is indirect. */
	ASSERT_TRUE((vq->desc[head].flags & VIRTQ_DESC_F_INDIRECT) != 0);
	ASSERT_EQ_U32(vq->desc[head].len, (UINT32)(3 * sizeof(VIRTQ_DESC)));

	VirtqSplitPublish(vq, head);
	DeviceConsumeAvailOne(vq, &dev_avail_idx, &dev_used_idx, sg, 3, 0x12345678);

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie_out, &len_out)));
	ASSERT_TRUE(cookie_out == (void *)0xCAFE);
	ASSERT_EQ_U32(len_out, 0x12345678);

	ASSERT_EQ_U16(vq->num_free, qsz);
	ASSERT_EQ_U16(vq->indirect_num_free, table_count);
	AssertRingFreeListIntact(vq);
	AssertIndirectFreeListIntact(vq);

	free(pool);
	free(ring);
	free(vq);
}

int main(void)
{
	TestDirectChainAddFree();
	TestWraparound();
	TestOutOfOrderCompletion();
	TestNeedEventBoundaryCases();
	TestRingLayoutEventIdx();
	TestInitRejectsMisalignedRing();
	TestEventIdxKickPrepare();
	TestNoNotifyKickPrepare();
	TestInterruptSuppressionNoEventIdx();
	TestInterruptSuppressionEventIdx();
	TestIndirectDescriptors();
	TestIndirectPoolExhaustionFallsBackToDirect();

	printf("virtqueue_split_test: all tests passed\n");
	return 0;
}
