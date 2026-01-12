/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtiosnd_contract.h"

static void test_device_cfg_values(void)
{
    TEST_ASSERT(VirtIoSndValidateDeviceCfgValues(/*Jacks=*/0, /*Streams=*/2, /*Chmaps=*/0) == TRUE);

    TEST_ASSERT(VirtIoSndValidateDeviceCfgValues(/*Jacks=*/1, /*Streams=*/2, /*Chmaps=*/0) == FALSE);
    TEST_ASSERT(VirtIoSndValidateDeviceCfgValues(/*Jacks=*/0, /*Streams=*/1, /*Chmaps=*/0) == FALSE);
    TEST_ASSERT(VirtIoSndValidateDeviceCfgValues(/*Jacks=*/0, /*Streams=*/2, /*Chmaps=*/1) == FALSE);
}

static void test_expected_queue_sizes(void)
{
    TEST_ASSERT_EQ_U32(VirtIoSndExpectedQueueSize(VIRTIOSND_QUEUE_INDEX_CONTROLQ), VIRTIOSND_QUEUE_SIZE_CONTROLQ);
    TEST_ASSERT_EQ_U32(VirtIoSndExpectedQueueSize(VIRTIOSND_QUEUE_INDEX_EVENTQ), VIRTIOSND_QUEUE_SIZE_EVENTQ);
    TEST_ASSERT_EQ_U32(VirtIoSndExpectedQueueSize(VIRTIOSND_QUEUE_INDEX_TXQ), VIRTIOSND_QUEUE_SIZE_TXQ);
    TEST_ASSERT_EQ_U32(VirtIoSndExpectedQueueSize(VIRTIOSND_QUEUE_INDEX_RXQ), VIRTIOSND_QUEUE_SIZE_RXQ);

    TEST_ASSERT_EQ_U32(VirtIoSndExpectedQueueSize((USHORT)0xFFFFu), 0);
}

int main(void)
{
    test_device_cfg_values();
    test_expected_queue_sizes();
    return 0;
}

