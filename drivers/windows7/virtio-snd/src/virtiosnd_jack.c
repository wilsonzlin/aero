/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "virtiosnd_jack.h"

_Use_decl_annotations_
VOID VirtIoSndJackStateInit(PVIRTIOSND_JACK_STATE State)
{
    ULONG i;

    if (State == NULL) {
        return;
    }

    for (i = 0; i < VIRTIOSND_JACK_STATE_COUNT; ++i) {
        State->Connected[i] = 1;
    }
}

_Use_decl_annotations_
BOOLEAN VirtIoSndJackStateUpdate(PVIRTIOSND_JACK_STATE State, ULONG JackId, BOOLEAN Connected)
{
    LONG v;
    LONG old;

    if (State == NULL) {
        return FALSE;
    }

    if (JackId >= VIRTIOSND_JACK_STATE_COUNT) {
        return FALSE;
    }

    v = Connected ? 1 : 0;
    old = InterlockedExchange(&State->Connected[JackId], v);
    return (old != v) ? TRUE : FALSE;
}

static __forceinline BOOLEAN VirtIoSndJackEventTypeToConnected(_In_ ULONG Type, _Out_ BOOLEAN* OutConnected)
{
    if (OutConnected != NULL) {
        *OutConnected = FALSE;
    }

    switch (Type) {
    case VIRTIO_SND_EVT_JACK_CONNECTED:
        if (OutConnected != NULL) {
            *OutConnected = TRUE;
        }
        return TRUE;
    case VIRTIO_SND_EVT_JACK_DISCONNECTED:
        if (OutConnected != NULL) {
            *OutConnected = FALSE;
        }
        return TRUE;
    default:
        return FALSE;
    }
}

_Use_decl_annotations_
BOOLEAN VirtIoSndJackStateProcessEventqBuffer(
    PVIRTIOSND_JACK_STATE State,
    const VOID* Buffer,
    UINT32 UsedLen,
    ULONG* OutJackId,
    BOOLEAN* OutConnected)
{
    VIRTIO_SND_EVENT evt;
    ULONG jackId;
    BOOLEAN connected;
    BOOLEAN changed;

    if (OutJackId != NULL) {
        *OutJackId = 0;
    }
    if (OutConnected != NULL) {
        *OutConnected = FALSE;
    }

    if (State == NULL || Buffer == NULL) {
        return FALSE;
    }

    if (UsedLen < (UINT32)sizeof(evt)) {
        return FALSE;
    }

    /*
     * The buffer comes from shared DMA memory and may not be naturally aligned
     * for struct access; copy it out before decoding.
     */
    RtlCopyMemory(&evt, Buffer, sizeof(evt));

    connected = FALSE;
    if (!VirtIoSndJackEventTypeToConnected(evt.type, &connected)) {
        return FALSE;
    }

    jackId = evt.data;
    if (jackId >= VIRTIOSND_JACK_STATE_COUNT) {
        return FALSE;
    }

    changed = VirtIoSndJackStateUpdate(State, jackId, connected);
    if (!changed) {
        return FALSE;
    }

    if (OutJackId != NULL) {
        *OutJackId = jackId;
    }
    if (OutConnected != NULL) {
        *OutConnected = connected;
    }

    return TRUE;
}

_Use_decl_annotations_
BOOLEAN VirtIoSndJackStateIsConnected(const VIRTIOSND_JACK_STATE* State, ULONG JackId)
{
    LONG v;

    if (State == NULL) {
        return TRUE;
    }

    if (JackId >= VIRTIOSND_JACK_STATE_COUNT) {
        return TRUE;
    }

    v = InterlockedCompareExchange((volatile LONG*)&State->Connected[JackId], 0, 0);
    return (v != 0) ? TRUE : FALSE;
}
