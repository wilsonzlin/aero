/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtiosnd_dma.h"

static void test_alloc_zeros_output_on_invalid_params(void)
{
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_DMA_BUFFER buf;
    NTSTATUS status;

    status = VirtIoSndDmaInit(NULL, &dma);
    TEST_ASSERT(status == STATUS_SUCCESS);

    memset(&buf, 0xA5, sizeof(buf));
    status = VirtIoSndAllocCommonBuffer(NULL, 16u, TRUE, &buf);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);
    TEST_ASSERT(buf.Va == NULL);
    TEST_ASSERT_EQ_U64(buf.DmaAddr, 0u);
    TEST_ASSERT_EQ_U64((uint64_t)buf.Size, 0u);
    TEST_ASSERT(buf.IsCommonBuffer == FALSE);
    TEST_ASSERT(buf.CacheEnabled == FALSE);

    memset(&buf, 0xA5, sizeof(buf));
    status = VirtIoSndAllocCommonBuffer(&dma, 0u, TRUE, &buf);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);
    TEST_ASSERT(buf.Va == NULL);
    TEST_ASSERT_EQ_U64(buf.DmaAddr, 0u);
    TEST_ASSERT_EQ_U64((uint64_t)buf.Size, 0u);
    TEST_ASSERT(buf.IsCommonBuffer == FALSE);
    TEST_ASSERT(buf.CacheEnabled == FALSE);

    VirtIoSndDmaUninit(&dma);
}

static void test_alloc_and_free_clears_buffer(void)
{
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_DMA_BUFFER buf;
    NTSTATUS status;

    status = VirtIoSndDmaInit(NULL, &dma);
    TEST_ASSERT(status == STATUS_SUCCESS);

    memset(&buf, 0xA5, sizeof(buf));
    status = VirtIoSndAllocCommonBuffer(&dma, 64u, FALSE, &buf);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(buf.Va != NULL);
    TEST_ASSERT_EQ_U64((uint64_t)buf.Size, 64u);
    TEST_ASSERT(buf.IsCommonBuffer == TRUE);
    TEST_ASSERT(buf.CacheEnabled == FALSE);

    VirtIoSndFreeCommonBuffer(&dma, &buf);
    TEST_ASSERT(buf.Va == NULL);
    TEST_ASSERT_EQ_U64(buf.DmaAddr, 0u);
    TEST_ASSERT_EQ_U64((uint64_t)buf.Size, 0u);
    TEST_ASSERT(buf.IsCommonBuffer == FALSE);
    TEST_ASSERT(buf.CacheEnabled == FALSE);

    VirtIoSndDmaUninit(&dma);
}

int main(void)
{
    test_alloc_zeros_output_on_invalid_params();
    test_alloc_and_free_clears_buffer();

    printf("virtiosnd_dma_stub_tests: PASS\n");
    return 0;
}

