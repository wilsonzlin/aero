/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtio_snd_proto.h"

static __forceinline VIRTIO_SND_EVENT_KIND VirtioSndEventKindFromType(_In_ ULONG type)
{
    switch (type) {
    case VIRTIO_SND_EVT_JACK_CONNECTED:
        return VIRTIO_SND_EVENT_KIND_JACK_CONNECTED;
    case VIRTIO_SND_EVT_JACK_DISCONNECTED:
        return VIRTIO_SND_EVENT_KIND_JACK_DISCONNECTED;
    case VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED:
        return VIRTIO_SND_EVENT_KIND_PCM_PERIOD_ELAPSED;
    case VIRTIO_SND_EVT_PCM_XRUN:
        return VIRTIO_SND_EVENT_KIND_PCM_XRUN;
    case VIRTIO_SND_EVT_CTL_NOTIFY:
        return VIRTIO_SND_EVENT_KIND_CTL_NOTIFY;
    default:
        return VIRTIO_SND_EVENT_KIND_UNKNOWN;
    }
}

NTSTATUS
VirtioSndStatusToNtStatus(_In_ ULONG virtio_status)
{
    switch (virtio_status) {
    case VIRTIO_SND_S_OK:
        return STATUS_SUCCESS;
    case VIRTIO_SND_S_BAD_MSG:
        return STATUS_INVALID_PARAMETER;
    case VIRTIO_SND_S_NOT_SUPP:
        return STATUS_NOT_SUPPORTED;
    case VIRTIO_SND_S_IO_ERR:
        // The device reports an I/O error or an invalid stream state; surface it as a device state
        // issue rather than a parameter error.
        return STATUS_INVALID_DEVICE_STATE;
    default:
#ifdef STATUS_DEVICE_PROTOCOL_ERROR
        return STATUS_DEVICE_PROTOCOL_ERROR;
#else
        return STATUS_UNSUCCESSFUL;
#endif
    }
}

PCSTR
VirtioSndStatusToString(_In_ ULONG virtio_status)
{
#if DBG
    switch (virtio_status) {
    case VIRTIO_SND_S_OK:
        return "OK";
    case VIRTIO_SND_S_BAD_MSG:
        return "BAD_MSG";
    case VIRTIO_SND_S_NOT_SUPP:
        return "NOT_SUPP";
    case VIRTIO_SND_S_IO_ERR:
        return "IO_ERR";
    default:
        return "UNKNOWN";
    }
#else
    UNREFERENCED_PARAMETER(virtio_status);
    return "";
#endif
}

_Use_decl_annotations_
NTSTATUS VirtioSndParseEvent(const void* Buffer, ULONG BufferLen, VIRTIO_SND_EVENT_PARSED* OutEvent)
{
    VIRTIO_SND_EVENT evt;

    if (OutEvent != NULL) {
        RtlZeroMemory(OutEvent, sizeof(*OutEvent));
    }

    if (Buffer == NULL || OutEvent == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (BufferLen < (ULONG)sizeof(VIRTIO_SND_EVENT)) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    /*
     * Don't assume any alignment of the wire buffer (tests exercise unaligned
     * access). Copy to a local packed structure.
     */
    RtlCopyMemory(&evt, Buffer, sizeof(evt));

    OutEvent->Type = evt.type;
    OutEvent->Data = evt.data;
    OutEvent->Kind = VirtioSndEventKindFromType(OutEvent->Type);
    return STATUS_SUCCESS;
}

PCSTR
VirtioSndEventTypeToString(_In_ ULONG virtio_event_type)
{
#if DBG
    switch (virtio_event_type) {
    case VIRTIO_SND_EVT_JACK_CONNECTED:
        return "JACK_CONNECTED";
    case VIRTIO_SND_EVT_JACK_DISCONNECTED:
        return "JACK_DISCONNECTED";
    case VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED:
        return "PCM_PERIOD_ELAPSED";
    case VIRTIO_SND_EVT_PCM_XRUN:
        return "PCM_XRUN";
    case VIRTIO_SND_EVT_CTL_NOTIFY:
        return "CTL_NOTIFY";
    default:
        return "UNKNOWN";
    }
#else
    UNREFERENCED_PARAMETER(virtio_event_type);
    return "";
#endif
}
