#include <ntddk.h>

#include "virtio_snd_proto.h"

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

