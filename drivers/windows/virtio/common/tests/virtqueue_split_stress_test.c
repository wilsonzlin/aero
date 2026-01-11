#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../virtqueue_split.h"

static void *AllocAlignedZero(size_t align, size_t size)
{
	void *raw;
	uintptr_t aligned_addr;
	size_t total;

	if (align == 0 || (align & (align - 1)) != 0) {
		return NULL;
	}

	total = size + align - 1 + sizeof(void *);
	raw = malloc(total);
	if (raw == NULL) {
		return NULL;
	}

	aligned_addr = ((uintptr_t)raw + sizeof(void *) + align - 1) & ~(uintptr_t)(align - 1);
	((void **)aligned_addr)[-1] = raw;
	memset((void *)aligned_addr, 0, size);
	return (void *)aligned_addr;
}

static void FreeAligned(void *p)
{
	if (p == NULL) {
		return;
	}
	free(((void **)p)[-1]);
}

typedef struct _PRNG {
	UINT64 state;
} PRNG;

static UINT64 PrngNext(PRNG *rng)
{
	/* xorshift64* */
	UINT64 x = rng->state;
	x ^= x >> 12;
	x ^= x << 25;
	x ^= x >> 27;
	rng->state = x;
	return x * 2685821657736338717ULL;
}

static UINT32 PrngU32(PRNG *rng) { return (UINT32)PrngNext(rng); }

static UINT32 PrngRange(PRNG *rng, UINT32 n) { return (n == 0) ? 0 : (PrngU32(rng) % n); }

static void ShuffleU16(PRNG *rng, UINT16 *a, size_t n)
{
	size_t i;
	if (n <= 1) {
		return;
	}
	for (i = n - 1; i > 0; i--) {
		size_t j = (size_t)PrngRange(rng, (UINT32)(i + 1));
		UINT16 tmp = a[i];
		a[i] = a[j];
		a[j] = tmp;
	}
}

#ifdef VIRTQ_DEBUG
static void LogLine(const char *line, void *ctx)
{
	(void)ctx;
	fprintf(stderr, "%s\n", line);
}
#endif

static void FailVq(const VIRTQ_SPLIT *vq, const char *file, int line, const char *expr)
{
	fprintf(stderr, "ASSERT failed at %s:%d: %s\n", file, line, expr);
#ifdef VIRTQ_DEBUG
	VirtqSplitDump(vq, LogLine, NULL);
#else
	(void)vq;
#endif
	abort();
}

#define ASSERT_VQ(vq, cond)                     \
	do {                                    \
		if (!(cond)) {                  \
			FailVq((vq), __FILE__, __LINE__, #cond); \
		}                               \
	} while (0)

#define ASSERT_TRUE(cond) ASSERT_VQ(NULL, cond)
#define ASSERT_EQ_U16(a, b) ASSERT_TRUE((UINT16)(a) == (UINT16)(b))
#define ASSERT_EQ_U32(a, b) ASSERT_TRUE((UINT32)(a) == (UINT32)(b))

typedef struct _VQ_CTX {
	UINT16 qsz;
	UINT32 align;
	BOOLEAN event_idx;
	BOOLEAN indirect;

	UINT16 indirect_table_count;
	UINT16 indirect_max_desc;

	VIRTQ_SPLIT *vq;
	void *ring;
	void *pool;

	/* Device-side indices */
	UINT16 dev_avail_idx;
	UINT16 dev_used_idx;

	/* Model/invariant tracking */
	UINT16 in_flight_desc;
	UINT16 indirect_in_flight;
	UINT8 *head_outstanding; /* [qsz] */
	UINT8 *head_uses_indirect; /* [qsz] */
	void **expected_cookie; /* [qsz] */
	UINT16 *desc_used; /* [qsz] */
} VQ_CTX;

static void ModelReset(VQ_CTX *ctx)
{
	ctx->in_flight_desc = 0;
	ctx->indirect_in_flight = 0;
	memset(ctx->head_outstanding, 0, ctx->qsz);
	memset(ctx->head_uses_indirect, 0, ctx->qsz);
	memset(ctx->expected_cookie, 0, sizeof(void *) * (size_t)ctx->qsz);
	memset(ctx->desc_used, 0, sizeof(UINT16) * (size_t)ctx->qsz);
}

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
		ASSERT_TRUE(count <= vq->qsz);
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
		ASSERT_TRUE(count <= vq->indirect_table_count);
		table = (VIRTQ_DESC *)((UINT8 *)vq->indirect_pool_va + (size_t)idx * vq->indirect_table_stride);
		idx = table[0].next;
	}

	ASSERT_EQ_U16(count, vq->indirect_num_free);
	free(seen);
}

static void AssertInvariants(VQ_CTX *ctx)
{
	VIRTQ_SPLIT *vq = ctx->vq;
	UINT16 i;

	ASSERT_VQ(vq, (UINT16)(vq->num_free + ctx->in_flight_desc) == ctx->qsz);
	AssertRingFreeListIntact(vq);

	for (i = 0; i < ctx->qsz; i++) {
		if (ctx->head_outstanding[i]) {
			ASSERT_VQ(vq, ctx->expected_cookie[i] != NULL);
			ASSERT_VQ(vq, vq->cookies[i] == ctx->expected_cookie[i]);
		} else {
			ASSERT_VQ(vq, vq->cookies[i] == NULL);
		}
	}

	if (vq->indirect_pool_va != NULL && vq->indirect_table_count != 0) {
		ASSERT_VQ(vq, (UINT16)(vq->indirect_num_free + ctx->indirect_in_flight) == vq->indirect_table_count);
		AssertIndirectFreeListIntact(vq);
		for (i = 0; i < ctx->qsz; i++) {
			if (ctx->head_uses_indirect[i]) {
				ASSERT_VQ(vq, vq->head_indirect[i] != VIRTQ_SPLIT_NO_DESC);
			} else {
				ASSERT_VQ(vq, vq->head_indirect[i] == VIRTQ_SPLIT_NO_DESC);
			}
		}
	}
}

static void CtxInitEx(VQ_CTX *ctx, UINT16 qsz, BOOLEAN event_idx, BOOLEAN indirect, UINT16 indirect_table_count)
{
	const UINT32 align = 16;
	const size_t dma_align = 16;

	size_t vq_bytes;
	size_t ring_bytes;
	size_t pool_bytes = 0;

	memset(ctx, 0, sizeof(*ctx));
	ctx->qsz = qsz;
	ctx->align = align;
	ctx->event_idx = event_idx ? TRUE : FALSE;
	ctx->indirect = indirect ? TRUE : FALSE;

	ctx->indirect_table_count = indirect ? indirect_table_count : 0;
	ctx->indirect_max_desc = indirect ? 16 : 0;

	vq_bytes = VirtqSplitStateSize(qsz);
	ring_bytes = VirtqSplitRingMemSize(qsz, align, ctx->event_idx);

	ctx->vq = (VIRTQ_SPLIT *)calloc(1, vq_bytes);
	ctx->ring = AllocAlignedZero(dma_align, ring_bytes);
	ASSERT_TRUE(ctx->vq != NULL);
	ASSERT_TRUE(ctx->ring != NULL);

	if (ctx->indirect && ctx->indirect_table_count != 0) {
		pool_bytes = (size_t)ctx->indirect_table_count * (size_t)ctx->indirect_max_desc * sizeof(VIRTQ_DESC);
		ctx->pool = AllocAlignedZero(dma_align, pool_bytes);
		ASSERT_TRUE(ctx->pool != NULL);
	}

	ASSERT_TRUE(NT_SUCCESS(VirtqSplitInit(ctx->vq, qsz, ctx->event_idx, ctx->indirect, ctx->ring,
					     (UINT64)(uintptr_t)ctx->ring, align, ctx->pool, (UINT64)(uintptr_t)ctx->pool,
					     ctx->indirect_table_count, ctx->indirect_max_desc)));

	/*
	 * For stress tests we want to exercise indirect paths deterministically.
	 * Force the policy to prefer indirect for any SG count > 0 as long as the
	 * pool is present.
	 */
	if (ctx->vq->indirect_pool_va != NULL) {
		ctx->vq->indirect_threshold = 0;
	}

	ctx->head_outstanding = (UINT8 *)calloc(qsz, 1);
	ctx->head_uses_indirect = (UINT8 *)calloc(qsz, 1);
	ctx->expected_cookie = (void **)calloc(qsz, sizeof(void *));
	ctx->desc_used = (UINT16 *)calloc(qsz, sizeof(UINT16));
	ASSERT_TRUE(ctx->head_outstanding != NULL);
	ASSERT_TRUE(ctx->head_uses_indirect != NULL);
	ASSERT_TRUE(ctx->expected_cookie != NULL);
	ASSERT_TRUE(ctx->desc_used != NULL);

	ctx->dev_avail_idx = 0;
	ctx->dev_used_idx = 0;
	ModelReset(ctx);
	AssertInvariants(ctx);
}

static void CtxInit(VQ_CTX *ctx, UINT16 qsz, BOOLEAN event_idx, BOOLEAN indirect)
{
	CtxInitEx(ctx, qsz, event_idx, indirect, indirect ? qsz : 0);
}

static void CtxDestroy(VQ_CTX *ctx)
{
	free(ctx->desc_used);
	free(ctx->expected_cookie);
	free(ctx->head_uses_indirect);
	free(ctx->head_outstanding);

	FreeAligned(ctx->pool);
	FreeAligned(ctx->ring);
	free(ctx->vq);
	memset(ctx, 0, sizeof(*ctx));
}

static void ModelOnAdd(VQ_CTX *ctx, UINT16 head, UINT16 sg_count, void *cookie)
{
	VIRTQ_SPLIT *vq = ctx->vq;
	UINT16 used_desc;
	BOOLEAN is_indirect;

	ASSERT_VQ(vq, head < ctx->qsz);
	ASSERT_VQ(vq, ctx->head_outstanding[head] == 0);

	is_indirect = (vq->head_indirect[head] != VIRTQ_SPLIT_NO_DESC) ? TRUE : FALSE;
	used_desc = is_indirect ? 1 : sg_count;

	ctx->head_outstanding[head] = 1;
	ctx->expected_cookie[head] = cookie;
	ctx->desc_used[head] = used_desc;
	ctx->in_flight_desc = (UINT16)(ctx->in_flight_desc + used_desc);

	ctx->head_uses_indirect[head] = (UINT8)(is_indirect != FALSE);
	if (is_indirect) {
		ctx->indirect_in_flight++;
	}
}

static void ModelOnPop(VQ_CTX *ctx, UINT16 head)
{
	VIRTQ_SPLIT *vq = ctx->vq;
	UINT16 used_desc;

	ASSERT_VQ(vq, head < ctx->qsz);
	ASSERT_VQ(vq, ctx->head_outstanding[head] != 0);

	used_desc = ctx->desc_used[head];
	ASSERT_VQ(vq, used_desc != 0);

	ctx->head_outstanding[head] = 0;
	ctx->expected_cookie[head] = NULL;
	ctx->desc_used[head] = 0;
	ctx->in_flight_desc = (UINT16)(ctx->in_flight_desc - used_desc);

	if (ctx->head_uses_indirect[head]) {
		ASSERT_VQ(vq, ctx->indirect_in_flight != 0);
		ctx->indirect_in_flight--;
	}
	ctx->head_uses_indirect[head] = 0;
}

static UINT16 DeviceConsumeAvail(VQ_CTX *ctx, UINT16 *out_heads, UINT16 max_heads)
{
	VIRTQ_SPLIT *vq = ctx->vq;
	UINT16 avail_idx;
	UINT16 count = 0;

	avail_idx = VirtioReadU16((volatile UINT16 *)&vq->avail->idx);
	while (ctx->dev_avail_idx != avail_idx) {
		UINT16 head;
		ASSERT_VQ(vq, count < max_heads);

		head = VirtioReadU16((volatile UINT16 *)&VirtqAvailRing(vq->avail)[(UINT16)(ctx->dev_avail_idx % vq->qsz)]);
		out_heads[count++] = head;
		ctx->dev_avail_idx++;
	}
	return count;
}

static void DeviceWriteUsed(VQ_CTX *ctx, UINT16 head, UINT32 len)
{
	VIRTQ_SPLIT *vq = ctx->vq;
	UINT16 slot;
	VIRTQ_USED_ELEM *used_ring;

	slot = (UINT16)(ctx->dev_used_idx % vq->qsz);
	used_ring = VirtqUsedRing(vq->used);
	VirtioWriteU32((volatile UINT32 *)&used_ring[slot].id, head);
	VirtioWriteU32((volatile UINT32 *)&used_ring[slot].len, len);
	ctx->dev_used_idx++;
}

static void DeviceCommitUsed(VQ_CTX *ctx)
{
	VIRTQ_SPLIT *vq = ctx->vq;
	VIRTIO_WMB();
	VirtioWriteU16((volatile UINT16 *)&vq->used->idx, ctx->dev_used_idx);
}

static BOOLEAN RefVringNeedEvent(UINT16 event, UINT16 new_idx, UINT16 old_idx)
{
	return (UINT16)(new_idx - event - 1) < (UINT16)(new_idx - old_idx);
}

static void ScenarioOutOfOrderCompletion(BOOLEAN event_idx, BOOLEAN indirect)
{
	const UINT16 qsz = 32;
	const UINT16 sg_count = 3;

	VQ_CTX ctx;
	VIRTQ_SG sg[sg_count];

	UINT16 heads[64];
	UINT16 n, i;

	UINT16 comp_heads[64];
	void *comp_cookie[64];
	UINT32 comp_len[64];

	PRNG rng;
	rng.state = 0x123456789ULL ^ (UINT64)(event_idx ? 1 : 0) ^ ((UINT64)(indirect ? 1 : 0) << 1);

	CtxInit(&ctx, qsz, event_idx, indirect);

	for (i = 0; i < sg_count; i++) {
		sg[i].addr = 0x1000 + (UINT64)i * 0x100;
		sg[i].len = 64 + (UINT32)i;
		sg[i].write = (i == (UINT16)(sg_count - 1)) ? TRUE : FALSE;
	}

	n = (UINT16)(qsz / (indirect ? 1 : sg_count));
	ASSERT_TRUE(n > 1);

	for (i = 0; i < n; i++) {
		UINT16 head;
		void *cookie = (void *)(uintptr_t)(0x1000u + i); /* non-NULL opaque */

		sg[0].addr = 0x200000 + (UINT64)i * 0x1000;
		ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head)));
		ModelOnAdd(&ctx, head, sg_count, cookie);
		VirtqSplitPublish(ctx.vq, head);
		AssertInvariants(&ctx);
	}

	ASSERT_EQ_U16(DeviceConsumeAvail(&ctx, heads, (UINT16)(sizeof(heads) / sizeof(heads[0]))), n);
	ShuffleU16(&rng, heads, n);

	for (i = 0; i < n; i++) {
		UINT32 len = 0xABC00000u + i;
		DeviceWriteUsed(&ctx, heads[i], len);
		comp_heads[i] = heads[i];
		comp_cookie[i] = ctx.expected_cookie[heads[i]];
		comp_len[i] = len;
	}
	DeviceCommitUsed(&ctx);
	AssertInvariants(&ctx);

	for (i = 0; i < n; i++) {
		void *cookie_out = NULL;
		UINT32 len_out = 0;
		ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
		ASSERT_TRUE(cookie_out == comp_cookie[i]);
		ASSERT_EQ_U32(len_out, comp_len[i]);
		ModelOnPop(&ctx, comp_heads[i]);
		AssertInvariants(&ctx);
	}

	ASSERT_TRUE(VirtqSplitGetUsed(ctx.vq, &comp_cookie[0], &comp_len[0]) == STATUS_NOT_FOUND);
	ASSERT_EQ_U16(ctx.vq->num_free, qsz);

	CtxDestroy(&ctx);
}

static void ScenarioRingFullBackpressure(BOOLEAN event_idx, BOOLEAN indirect)
{
	const UINT16 qsz = 32;
	const UINT16 sg_count = 3;

	VQ_CTX ctx;
	VIRTQ_SG sg[sg_count];
	UINT16 avail_heads[32];
	UINT16 count = 0;
	UINT16 i;

	PRNG rng;
	rng.state = 0xBADC0DEULL ^ (UINT64)(event_idx ? 1 : 0) ^ ((UINT64)(indirect ? 1 : 0) << 1);

	CtxInit(&ctx, qsz, event_idx, indirect);

	for (i = 0; i < sg_count; i++) {
		sg[i].addr = 0x4000 + (UINT64)i * 0x100;
		sg[i].len = 128 + (UINT32)i;
		sg[i].write = (i == (UINT16)(sg_count - 1)) ? TRUE : FALSE;
	}

	for (;;) {
		UINT16 head;
		void *cookie = (void *)(uintptr_t)(0x2000u + count);
		NTSTATUS st;

		sg[0].addr = 0x800000 + (UINT64)count * 0x1000;
		st = VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head);
		if (st == STATUS_INSUFFICIENT_RESOURCES) {
			break;
		}
		ASSERT_TRUE(NT_SUCCESS(st));

		ModelOnAdd(&ctx, head, sg_count, cookie);
		VirtqSplitPublish(ctx.vq, head);
		count++;
		ASSERT_TRUE(count <= qsz);
		AssertInvariants(&ctx);
	}

	ASSERT_TRUE(count > 0);

	/* Device consumes all published heads. */
	ASSERT_EQ_U16(DeviceConsumeAvail(&ctx, avail_heads, count), count);

	/* Complete half of them, then verify we can add again. */
	ShuffleU16(&rng, avail_heads, count);
	{
		UINT16 complete_n = (UINT16)(count / 2);
		UINT16 *exp_head = (UINT16 *)calloc(complete_n, sizeof(UINT16));
		void **exp_cookie = (void **)calloc(complete_n, sizeof(void *));
		UINT32 *exp_len = (UINT32 *)calloc(complete_n, sizeof(UINT32));

		ASSERT_TRUE(exp_head && exp_cookie && exp_len);

		for (i = 0; i < complete_n; i++) {
			exp_head[i] = avail_heads[i];
			exp_cookie[i] = ctx.expected_cookie[avail_heads[i]];
			exp_len[i] = 0xEE00u + i;
			DeviceWriteUsed(&ctx, exp_head[i], exp_len[i]);
		}
		DeviceCommitUsed(&ctx);

		for (i = 0; i < complete_n; i++) {
			void *cookie_out = NULL;
			UINT32 len_out = 0;
			ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
			ASSERT_TRUE(cookie_out == exp_cookie[i]);
			ASSERT_EQ_U32(len_out, exp_len[i]);
			ModelOnPop(&ctx, exp_head[i]);
			AssertInvariants(&ctx);
		}

		/* Adds should succeed again after completions. */
		for (i = 0; i < complete_n; i++) {
			UINT16 head;
			void *cookie = (void *)(uintptr_t)(0x3000u + i);
			ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head)));
			ModelOnAdd(&ctx, head, sg_count, cookie);
			VirtqSplitPublish(ctx.vq, head);
			AssertInvariants(&ctx);
		}

		/* Device consumes the newly published heads as well. */
		{
			UINT16 new_heads[32];
			ASSERT_EQ_U16(DeviceConsumeAvail(&ctx, new_heads, (UINT16)(sizeof(new_heads) / sizeof(new_heads[0]))),
				      complete_n);
		}

		free(exp_len);
		free(exp_cookie);
		free(exp_head);
	}

	/* Drain everything left so invariants end in a clean state. */
	{
		UINT16 outstanding[32];
		UINT16 out_n = 0;
		for (i = 0; i < ctx.qsz; i++) {
			if (ctx.head_outstanding[i]) {
				outstanding[out_n++] = i;
			}
		}
		ShuffleU16(&rng, outstanding, out_n);
		for (i = 0; i < out_n; i++) {
			DeviceWriteUsed(&ctx, outstanding[i], 0xDD00u + i);
		}
		DeviceCommitUsed(&ctx);
		for (i = 0; i < out_n; i++) {
			void *cookie_out = NULL;
			UINT32 len_out = 0;
			ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
			ModelOnPop(&ctx, outstanding[i]);
		}
	}
	AssertInvariants(&ctx);
	ASSERT_EQ_U16(ctx.vq->num_free, qsz);

	CtxDestroy(&ctx);
}

static void ScenarioIndirectPoolExhaustionFallback(BOOLEAN event_idx)
{
	/*
	 * Exercise a tricky corner of the indirect feature: the driver may have
	 * negotiated indirect descriptors, but the indirect table pool can be
	 * smaller than the ring size. In that case VirtqSplitAddBuffer() must fall
	 * back to direct chains without corrupting head_indirect[] bookkeeping.
	 */
	const UINT16 qsz = 8;
	const UINT16 sg_count = 3;
	const UINT16 pool_tables = 1;

	VQ_CTX ctx;
	VIRTQ_SG sg[sg_count];
	UINT16 heads[4];
	UINT16 i;

	PRNG rng;
	rng.state = 0x51515555ULL ^ (UINT64)(event_idx ? 1 : 0);

	CtxInitEx(&ctx, qsz, event_idx, TRUE, pool_tables);
	ASSERT_TRUE(ctx.vq->indirect_pool_va != NULL);
	ASSERT_EQ_U16(ctx.vq->indirect_table_count, pool_tables);

	for (i = 0; i < sg_count; i++) {
		sg[i].addr = 0x900000 + (UINT64)i * 0x100;
		sg[i].len = 32 + (UINT32)i;
		sg[i].write = (i == (UINT16)(sg_count - 1)) ? TRUE : FALSE;
	}

	/* First buffer should use indirect (pool has 1 table). */
	{
		void *cookie = (void *)(uintptr_t)0x60000001u;
		UINT16 head;
		ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head)));
		ModelOnAdd(&ctx, head, sg_count, cookie);
		ASSERT_VQ(ctx.vq, ctx.vq->head_indirect[head] != VIRTQ_SPLIT_NO_DESC);
		VirtqSplitPublish(ctx.vq, head);
		AssertInvariants(&ctx);
	}

	/* Subsequent buffers must use direct chains because the pool is exhausted. */
	for (i = 0; i < 2; i++) {
		void *cookie = (void *)(uintptr_t)(0x60000010u + i);
		UINT16 head;
		ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head)));
		ModelOnAdd(&ctx, head, sg_count, cookie);
		ASSERT_VQ(ctx.vq, ctx.vq->head_indirect[head] == VIRTQ_SPLIT_NO_DESC);
		VirtqSplitPublish(ctx.vq, head);
		AssertInvariants(&ctx);
	}

	/* Device consumes + completes all published buffers out of order. */
	{
		UINT16 consumed = DeviceConsumeAvail(&ctx, heads, (UINT16)(sizeof(heads) / sizeof(heads[0])));
		void *exp_cookie[4];
		UINT32 exp_len[4];
		UINT16 exp_head[4];

		ASSERT_EQ_U16(consumed, 3);
		ShuffleU16(&rng, heads, consumed);

		for (i = 0; i < consumed; i++) {
			exp_head[i] = heads[i];
			exp_cookie[i] = ctx.expected_cookie[heads[i]];
			exp_len[i] = 0x11110000u + i;
			DeviceWriteUsed(&ctx, heads[i], exp_len[i]);
		}
		DeviceCommitUsed(&ctx);

		for (i = 0; i < consumed; i++) {
			void *cookie_out = NULL;
			UINT32 len_out = 0;
			ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
			ASSERT_TRUE(cookie_out == exp_cookie[i]);
			ASSERT_EQ_U32(len_out, exp_len[i]);
			ModelOnPop(&ctx, exp_head[i]);
			AssertInvariants(&ctx);
		}
	}

	ASSERT_EQ_U16(ctx.vq->num_free, qsz);
	ASSERT_EQ_U16(ctx.vq->indirect_num_free, pool_tables);

	/* Verify an indirect buffer can be posted again after the table is freed. */
	{
		UINT16 head;
		void *cookie = (void *)(uintptr_t)0x60000099u;
		ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head)));
		ModelOnAdd(&ctx, head, sg_count, cookie);
		ASSERT_VQ(ctx.vq, ctx.vq->head_indirect[head] != VIRTQ_SPLIT_NO_DESC);
		VirtqSplitPublish(ctx.vq, head);

		ASSERT_EQ_U16(DeviceConsumeAvail(&ctx, heads, (UINT16)(sizeof(heads) / sizeof(heads[0]))), 1);
		DeviceWriteUsed(&ctx, heads[0], 0xCAFE);
		DeviceCommitUsed(&ctx);

		void *cookie_out = NULL;
		UINT32 len_out = 0;
		ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
		ASSERT_TRUE(cookie_out == cookie);
		ASSERT_EQ_U32(len_out, 0xCAFE);
		ModelOnPop(&ctx, heads[0]);
		AssertInvariants(&ctx);
	}

	ASSERT_EQ_U16(ctx.vq->num_free, qsz);
	ASSERT_EQ_U16(ctx.vq->indirect_num_free, pool_tables);
	CtxDestroy(&ctx);
}

static void ScenarioWraparoundTorture(BOOLEAN event_idx, BOOLEAN indirect)
{
	const UINT16 qsz = 32;
	const UINT16 sg_count = 3;
	const UINT16 start = 0xFFF0u;
	const UINT32 ops = 100000;

	VQ_CTX ctx;
	VIRTQ_SG sg[sg_count];
	UINT16 outstanding[64];
	UINT16 outstanding_count = 0;
	UINT32 cookie_counter = 1;
	UINT32 step;

	PRNG rng;
	rng.state = 0xDEADBEEFULL ^ (UINT64)(event_idx ? 1 : 0) ^ ((UINT64)(indirect ? 1 : 0) << 1);

	CtxInit(&ctx, qsz, event_idx, indirect);

	/* Force indices near wrap boundary (both device-visible and driver shadows). */
	ctx.vq->avail_idx = start;
	ctx.vq->last_used_idx = start;
	ctx.vq->num_added = 0;
	VirtioWriteU16((volatile UINT16 *)&ctx.vq->avail->idx, start);
	VirtioWriteU16((volatile UINT16 *)&ctx.vq->used->idx, start);
	ctx.dev_avail_idx = start;
	ctx.dev_used_idx = start;
	if (ctx.vq->event_idx) {
		VirtioWriteU16(VirtqAvailUsedEvent(ctx.vq->avail, ctx.vq->qsz), start);
		VirtioWriteU16(VirtqUsedAvailEvent(ctx.vq->used, ctx.vq->qsz), start);
	}
	AssertInvariants(&ctx);

	for (step = 0; step < ops; step++) {
		UINT16 i;
		UINT16 new_heads[4];
		UINT16 new_n;
		UINT16 complete_n;
		UINT16 exp_head[4];
		void *exp_cookie[4];
		UINT32 exp_len[4];

		for (i = 0; i < sg_count; i++) {
			sg[i].addr = 0x100000 + (UINT64)i * 0x100 + (UINT64)step * 0x1000;
			sg[i].len = 64 + (UINT32)i;
			sg[i].write = (i == (UINT16)(sg_count - 1)) ? TRUE : FALSE;
		}

		/* Prefer adds but accept backpressure. */
		if (PrngRange(&rng, 100) < 70) {
			UINT16 head;
			void *cookie = (void *)(uintptr_t)(0x40000000u + cookie_counter++);
			NTSTATUS st = VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head);
			if (NT_SUCCESS(st)) {
				ModelOnAdd(&ctx, head, sg_count, cookie);
				VirtqSplitPublish(ctx.vq, head);
			} else {
				ASSERT_TRUE(st == STATUS_INSUFFICIENT_RESOURCES);
			}
		}

		/* Device consumes any new avail entries. */
		new_n = DeviceConsumeAvail(&ctx, new_heads, (UINT16)(sizeof(new_heads) / sizeof(new_heads[0])));
		for (i = 0; i < new_n; i++) {
			ASSERT_TRUE(outstanding_count < (UINT16)(sizeof(outstanding) / sizeof(outstanding[0])));
			outstanding[outstanding_count++] = new_heads[i];
		}

		/* Complete up to 2 outstanding, out-of-order. */
		complete_n = (UINT16)((outstanding_count == 0) ? 0 : PrngRange(&rng, 3));
		UINT16 completed = 0;
		for (i = 0; i < complete_n && outstanding_count != 0; i++) {
			const UINT16 pick = (UINT16)PrngRange(&rng, outstanding_count);
			const UINT16 head = outstanding[pick];
			outstanding[pick] = outstanding[outstanding_count - 1];
			outstanding_count--;

			{
				UINT32 len = 0x70000000u + (UINT32)(step & 0xFFFFu);
				exp_head[completed] = head;
				exp_cookie[completed] = ctx.expected_cookie[head];
				exp_len[completed] = len;
				DeviceWriteUsed(&ctx, head, len);
			}
			completed++;
		}
		if (completed != 0) {
			DeviceCommitUsed(&ctx);
		}

		for (i = 0; i < completed; i++) {
			void *cookie_out = NULL;
			UINT32 len_out = 0;
			ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
			ASSERT_TRUE(cookie_out == exp_cookie[i]);
			ASSERT_EQ_U32(len_out, exp_len[i]);
			ModelOnPop(&ctx, exp_head[i]);
		}

		AssertInvariants(&ctx);

		/* Occasionally exercise kick decision logic under event-idx. */
		if (ctx.vq->event_idx && ((step & 0x3FFu) == 0)) {
			*VirtqUsedAvailEvent(ctx.vq->used, ctx.vq->qsz) = (UINT16)PrngU32(&rng);
			(void)VirtqSplitKickPrepare(ctx.vq);
			VirtqSplitKickCommit(ctx.vq);
		}
	}

	/* Drain everything that remains outstanding. */
	ShuffleU16(&rng, outstanding, outstanding_count);
	for (UINT16 i = 0; i < outstanding_count; i++) {
		DeviceWriteUsed(&ctx, outstanding[i], 0);
	}
	DeviceCommitUsed(&ctx);
	for (UINT16 i = 0; i < outstanding_count; i++) {
		void *cookie_out = NULL;
		UINT32 len_out = 0;
		ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
		ModelOnPop(&ctx, outstanding[i]);
	}
	AssertInvariants(&ctx);
	ASSERT_EQ_U16(ctx.vq->num_free, qsz);

	CtxDestroy(&ctx);
}

static void ScenarioNotifyDecisionSanity(BOOLEAN event_idx, BOOLEAN indirect)
{
	const UINT16 qsz = 32;
	const UINT16 sg_count = 1;
	const UINT16 start = 0xFFF0u;

	VQ_CTX ctx;
	VIRTQ_SG sg[1];
	PRNG rng;
	rng.state = 0xC0FFEEULL ^ (UINT64)(event_idx ? 1 : 0) ^ ((UINT64)(indirect ? 1 : 0) << 1);

	CtxInit(&ctx, qsz, event_idx, indirect);

	sg[0].addr = 0x11110000;
	sg[0].len = 16;
	sg[0].write = TRUE;

	for (UINT16 iter = 0; iter < 256; iter++) {
		UINT16 base = (UINT16)(start + (UINT16)PrngRange(&rng, 0x40));
		UINT16 batch = (UINT16)(1 + (UINT16)PrngRange(&rng, 4));
		UINT16 heads[8];
		UINT16 got_heads;

		VirtqSplitReset(ctx.vq);
		ModelReset(&ctx);

		ctx.vq->avail_idx = base;
		ctx.vq->last_used_idx = base;
		ctx.vq->num_added = 0;
		VirtioWriteU16((volatile UINT16 *)&ctx.vq->avail->idx, base);
		VirtioWriteU16((volatile UINT16 *)&ctx.vq->used->idx, base);
		ctx.dev_avail_idx = base;
		ctx.dev_used_idx = base;

		if (ctx.vq->event_idx) {
			UINT16 event = (UINT16)(base + (UINT16)PrngRange(&rng, 0x40));
			VirtioWriteU16(VirtqUsedAvailEvent(ctx.vq->used, ctx.vq->qsz), event);
		} else {
			/* Flip NO_NOTIFY randomly to ensure the non-event-idx path respects it. */
			UINT16 flags = (PrngRange(&rng, 2) == 0) ? VIRTQ_USED_F_NO_NOTIFY : 0;
			VirtioWriteU16((volatile UINT16 *)&ctx.vq->used->flags, flags);
		}

		for (UINT16 i = 0; i < batch; i++) {
			UINT16 head;
			void *cookie = (void *)(uintptr_t)(0x50000000u + (UINT32)i);
			ASSERT_TRUE(NT_SUCCESS(VirtqSplitAddBuffer(ctx.vq, sg, sg_count, cookie, &head)));
			ModelOnAdd(&ctx, head, sg_count, cookie);
			VirtqSplitPublish(ctx.vq, head);
			AssertInvariants(&ctx);
		}

		if (ctx.vq->event_idx) {
			UINT16 event = VirtioReadU16(VirtqUsedAvailEvent(ctx.vq->used, ctx.vq->qsz));
			UINT16 new_avail = ctx.vq->avail_idx;
			UINT16 old_avail = (UINT16)(new_avail - ctx.vq->num_added);
			BOOLEAN expected = RefVringNeedEvent(event, new_avail, old_avail);
			BOOLEAN got = VirtqSplitKickPrepare(ctx.vq);
			ASSERT_TRUE(got == expected);
		} else {
			UINT16 flags = VirtioReadU16((volatile UINT16 *)&ctx.vq->used->flags);
			BOOLEAN expected = (flags & VIRTQ_USED_F_NO_NOTIFY) ? FALSE : TRUE;
			BOOLEAN got = VirtqSplitKickPrepare(ctx.vq);
			ASSERT_TRUE(got == expected);
		}
		VirtqSplitKickCommit(ctx.vq);

		/* Drain published buffers. */
		got_heads = DeviceConsumeAvail(&ctx, heads, (UINT16)(sizeof(heads) / sizeof(heads[0])));
		ASSERT_EQ_U16(got_heads, batch);
		for (UINT16 i = 0; i < batch; i++) {
			DeviceWriteUsed(&ctx, heads[i], 0xAA00u + i);
		}
		DeviceCommitUsed(&ctx);
		for (UINT16 i = 0; i < batch; i++) {
			void *cookie_out = NULL;
			UINT32 len_out = 0;
			ASSERT_TRUE(NT_SUCCESS(VirtqSplitGetUsed(ctx.vq, &cookie_out, &len_out)));
			ModelOnPop(&ctx, heads[i]);
			AssertInvariants(&ctx);
		}
	}

	CtxDestroy(&ctx);
}

int main(void)
{
	const struct {
		BOOLEAN event_idx;
		BOOLEAN indirect;
	} matrix[] = {
		{FALSE, FALSE},
		{TRUE, FALSE},
		{FALSE, TRUE},
		{TRUE, TRUE},
	};

	for (size_t i = 0; i < sizeof(matrix) / sizeof(matrix[0]); i++) {
		const BOOLEAN event_idx = matrix[i].event_idx;
		const BOOLEAN indirect = matrix[i].indirect;

		ScenarioNotifyDecisionSanity(event_idx, indirect);
		ScenarioOutOfOrderCompletion(event_idx, indirect);
		ScenarioRingFullBackpressure(event_idx, indirect);
		if (indirect) {
			ScenarioIndirectPoolExhaustionFallback(event_idx);
		}
		ScenarioWraparoundTorture(event_idx, indirect);
	}

	printf("virtqueue_split_stress_test: all tests passed\n");
	return 0;
}
