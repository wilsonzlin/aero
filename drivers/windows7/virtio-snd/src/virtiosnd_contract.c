/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_jack_ids.h"
#include "virtiosnd_contract.h"

BOOLEAN VirtIoSndValidateDeviceCfgValues(_In_ ULONG Jacks, _In_ ULONG Streams, _In_ ULONG Chmaps)
{
    /*
     * The Aero Windows 7 virtio-snd contract v1 originally specified Jacks=0.
     *
     * The driver now tolerates Jacks=2 so that host/device models can emit
     * standard virtio-snd JACK eventq notifications while still matching the
     * fixed two-endpoint topology exposed by this driver.
     */
    if (Streams != 2u || Chmaps != 0u) {
        return FALSE;
    }

    return (Jacks == 0u || Jacks == VIRTIOSND_JACK_ID_COUNT) ? TRUE : FALSE;
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
