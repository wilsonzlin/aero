#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

#include "../virtio_sg_pfn.h"

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
#define ASSERT_EQ_STATUS(a, b) ASSERT_TRUE((UINT32)(a) == (UINT32)(b))

static void TestContiguousCoalesce(void)
{
	UINT64 pfns[3] = { 0x100, 0x101, 0x102 };
	VIRTQ_SG sg[4];
	UINT16 count = 0;
	NTSTATUS status;

	status = VirtioSgBuildFromPfns(pfns, 3, 0, PAGE_SIZE * 3u, TRUE, sg, 4, &count);
	ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
	ASSERT_EQ_U16(count, 1);
	ASSERT_EQ_U64(sg[0].addr, ((UINT64)0x100u << PAGE_SHIFT));
	ASSERT_EQ_U32(sg[0].len, (UINT32)(PAGE_SIZE * 3u));
	ASSERT_TRUE(sg[0].write != FALSE);
}

static void TestNonContiguousSplits(void)
{
	UINT64 pfns[3] = { 0x100, 0x102, 0x103 };
	VIRTQ_SG sg[4];
	UINT16 count = 0;
	NTSTATUS status;

	status = VirtioSgBuildFromPfns(pfns, 3, 0, PAGE_SIZE * 3u, FALSE, sg, 4, &count);
	ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
	ASSERT_EQ_U16(count, 2);

	ASSERT_EQ_U64(sg[0].addr, ((UINT64)0x100u << PAGE_SHIFT));
	ASSERT_EQ_U32(sg[0].len, (UINT32)PAGE_SIZE);
	ASSERT_TRUE(sg[0].write == FALSE);

	ASSERT_EQ_U64(sg[1].addr, ((UINT64)0x102u << PAGE_SHIFT));
	ASSERT_EQ_U32(sg[1].len, (UINT32)(PAGE_SIZE * 2u));
	ASSERT_TRUE(sg[1].write == FALSE);
}

static void TestFirstPageOffsetCoalescesAcrossBoundary(void)
{
	UINT64 pfns[2] = { 0x200, 0x201 };
	VIRTQ_SG sg[2];
	UINT16 count = 0;
	size_t len = (PAGE_SIZE - 100u) + 50u;
	NTSTATUS status;

	status = VirtioSgBuildFromPfns(pfns, 2, 100u, len, TRUE, sg, 2, &count);
	ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
	ASSERT_EQ_U16(count, 1);
	ASSERT_EQ_U64(sg[0].addr, (((UINT64)0x200u << PAGE_SHIFT) + 100u));
	ASSERT_EQ_U32(sg[0].len, (UINT32)len);
	ASSERT_TRUE(sg[0].write != FALSE);
}

static void TestMultiPagePartialLast(void)
{
	UINT64 pfns[3] = { 0x300, 0x301, 0x302 };
	VIRTQ_SG sg[2];
	UINT16 count = 0;
	size_t len = (PAGE_SIZE * 2u) + 123u;
	NTSTATUS status;

	status = VirtioSgBuildFromPfns(pfns, 3, 0, len, TRUE, sg, 2, &count);
	ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
	ASSERT_EQ_U16(count, 1);
	ASSERT_EQ_U64(sg[0].addr, ((UINT64)0x300u << PAGE_SHIFT));
	ASSERT_EQ_U32(sg[0].len, (UINT32)len);
}

static void TestBoundaryCases(void)
{
	{
		UINT64 pfns[2] = { 0x400, 0x401 };
		VIRTQ_SG sg[2];
		UINT16 count = 0;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(pfns, 2, 0, PAGE_SIZE, TRUE, sg, 2, &count);
		ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
		ASSERT_EQ_U16(count, 1);
		ASSERT_EQ_U64(sg[0].addr, ((UINT64)0x400u << PAGE_SHIFT));
		ASSERT_EQ_U32(sg[0].len, (UINT32)PAGE_SIZE);
	}

	{
		UINT64 pfns[1] = { 0x500 };
		VIRTQ_SG sg[1];
		UINT16 count = 0;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(pfns, 1, PAGE_SIZE - 1u, 1u, TRUE, sg, 1, &count);
		ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
		ASSERT_EQ_U16(count, 1);
		ASSERT_EQ_U64(sg[0].addr, (((UINT64)0x500u << PAGE_SHIFT) + (PAGE_SIZE - 1u)));
		ASSERT_EQ_U32(sg[0].len, 1u);
	}

	{
		UINT64 pfns[2] = { 0x600, 0x601 };
		VIRTQ_SG sg[1];
		UINT16 count = 0;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(pfns, 2, PAGE_SIZE - 1u, 2u, TRUE, sg, 1, &count);
		ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
		ASSERT_EQ_U16(count, 1);
		ASSERT_EQ_U64(sg[0].addr, (((UINT64)0x600u << PAGE_SHIFT) + (PAGE_SIZE - 1u)));
		ASSERT_EQ_U32(sg[0].len, 2u);
	}
}

static void TestBufferTooSmall(void)
{
	UINT64 pfns[3] = { 1, 3, 5 };
	VIRTQ_SG sg[1];
	UINT16 count = 0;
	NTSTATUS status;

	status = VirtioSgBuildFromPfns(pfns, 3, 0, PAGE_SIZE * 3u, TRUE, sg, 1, &count);
	ASSERT_EQ_STATUS(status, STATUS_BUFFER_TOO_SMALL);
	ASSERT_EQ_U16(count, 3);

	ASSERT_EQ_U64(sg[0].addr, ((UINT64)1u << PAGE_SHIFT));
	ASSERT_EQ_U32(sg[0].len, (UINT32)PAGE_SIZE);
	ASSERT_TRUE(sg[0].write != FALSE);
}

static void TestLenClampedToU32(void)
{
#if SIZE_MAX > 0xFFFFFFFFu
	const size_t len = ((size_t)0xFFFFFFFFu) + 1u; /* 4GiB + 1 */
	const UINT32 pfn_count = (UINT32)((len + (PAGE_SIZE - 1)) / PAGE_SIZE);
	UINT64 *pfns;
	VIRTQ_SG sg[3];
	UINT16 count = 0;
	NTSTATUS status;
	UINT32 i;

	pfns = (UINT64 *)calloc((size_t)pfn_count, sizeof(UINT64));
	ASSERT_TRUE(pfns != NULL);
	for (i = 0; i < pfn_count; i++) {
		pfns[i] = (UINT64)0x1000u + (UINT64)i;
	}

	status = VirtioSgBuildFromPfns(pfns, pfn_count, 0, len, TRUE, sg, 3, &count);
	ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
	ASSERT_EQ_U16(count, 2);

	ASSERT_EQ_U64(sg[0].addr, ((UINT64)0x1000u << PAGE_SHIFT));
	ASSERT_EQ_U32(sg[0].len, 0xFFFFFFFFu);

	ASSERT_EQ_U64(sg[1].addr, sg[0].addr + (UINT64)sg[0].len);
	ASSERT_EQ_U32(sg[1].len, 1u);

	free(pfns);
#endif
}

static void TestSizingCallNoOutput(void)
{
	UINT64 pfns[2] = { 0x700, 0x702 };
	UINT16 count = 0;
	NTSTATUS status;

	status = VirtioSgBuildFromPfns(pfns, 2, 0, PAGE_SIZE * 2u, TRUE, NULL, 0, &count);
	ASSERT_EQ_STATUS(status, STATUS_BUFFER_TOO_SMALL);
	ASSERT_EQ_U16(count, 2);
}

static void TestInvalidParams(void)
{
	{
		UINT64 pfns[1] = { 0x800 };
		VIRTQ_SG sg[1];
		UINT16 count = 0;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(pfns, 1, PAGE_SIZE, 1u, TRUE, sg, 1, &count);
		ASSERT_EQ_STATUS(status, STATUS_INVALID_PARAMETER);
	}

	{
		UINT64 pfns[1] = { 0x800 };
		VIRTQ_SG sg[1];
		UINT16 count = 0;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(pfns, 1, 0, PAGE_SIZE + 1u, TRUE, sg, 1, &count);
		ASSERT_EQ_STATUS(status, STATUS_INVALID_PARAMETER);
	}

	{
		UINT64 pfns[1] = { 0x800 };
		UINT16 count = 0;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(pfns, 1, 0, 1u, TRUE, NULL, 1, &count);
		ASSERT_EQ_STATUS(status, STATUS_INVALID_PARAMETER);
	}

	{
		VIRTQ_SG sg[1];
		UINT16 count = 0;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(NULL, 0, 0, 1u, TRUE, sg, 1, &count);
		ASSERT_EQ_STATUS(status, STATUS_INVALID_PARAMETER);
	}

	{
		UINT16 count = 0xBEEF;
		NTSTATUS status;

		status = VirtioSgBuildFromPfns(NULL, 0, 0, 0, TRUE, NULL, 0, &count);
		ASSERT_EQ_STATUS(status, STATUS_SUCCESS);
		ASSERT_EQ_U16(count, 0);
	}
}

int main(void)
{
	TestContiguousCoalesce();
	TestNonContiguousSplits();
	TestFirstPageOffsetCoalescesAcrossBoundary();
	TestMultiPagePartialLast();
	TestBoundaryCases();
	TestBufferTooSmall();
	TestLenClampedToU32();
	TestSizingCallNoOutput();
	TestInvalidParams();

	printf("virtio_sg_pfn_test: all tests passed\n");
	return 0;
}
