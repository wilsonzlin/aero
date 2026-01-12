/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_contract.h"

BOOLEAN VirtIoSndValidateDeviceCfgValues(_In_ ULONG Jacks, _In_ ULONG Streams, _In_ ULONG Chmaps)
{
    return (Jacks == 0u && Streams == 2u && Chmaps == 0u) ? TRUE : FALSE;
}

USHORT VirtIoSndExpectedQueueSize(_In_ USHORT QueueIndex)
{
    switch (QueueIndex) {
    case VIRTIOSND_QUEUE_INDEX_CONTROLQ:
        return VIRTIOSND_QUEUE_SIZE_CONTROLQ;
    case VIRTIOSND_QUEUE_INDEX_EVENTQ:
        return VIRTIOSND_QUEUE_SIZE_EVENTQ;
    case VIRTIOSND_QUEUE_INDEX_TXQ:
        return VIRTIOSND_QUEUE_SIZE_TXQ;
    case VIRTIOSND_QUEUE_INDEX_RXQ:
        return VIRTIOSND_QUEUE_SIZE_RXQ;
    default:
        return 0;
    }
}

