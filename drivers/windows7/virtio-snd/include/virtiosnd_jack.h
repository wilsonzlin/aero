/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd_jack_ids.h"
#include "virtio_snd_proto.h"

/*
 * virtio-snd jack state tracking used by the PortCls topology miniport.
 *
 * The Windows 7 virtio-snd driver exposes two fixed endpoints (speaker + microphone).
 * Map those onto two jack IDs so virtio-snd eventq JACK events can toggle
 * KSPROPERTY_JACK_DESCRIPTION::IsConnected at runtime.
 *
 * Note: Jack IDs are defined by `include/virtiosnd_jack_ids.h` (VIRTIOSND_JACK_ID_*).
 * This module simply tracks jacks indexed by those same IDs:
 *  - Jack 0: speaker/output
 *  - Jack 1: microphone/input
 */
  
#define VIRTIOSND_JACK_STATE_COUNT VIRTIOSND_JACK_ID_COUNT

typedef struct _VIRTIOSND_JACK_STATE {
    volatile LONG Connected[VIRTIOSND_JACK_STATE_COUNT];
} VIRTIOSND_JACK_STATE, *PVIRTIOSND_JACK_STATE;

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Initialize all jacks to "connected" so behaviour matches the previous
 * always-connected topology when the device does not emit jack events.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtIoSndJackStateInit(_Out_ PVIRTIOSND_JACK_STATE State);

/*
 * Update jack connection state.
 *
 * Returns TRUE only if JackId is known and the stored state changed.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
BOOLEAN VirtIoSndJackStateUpdate(_Inout_ PVIRTIOSND_JACK_STATE State, _In_ ULONG JackId, _In_ BOOLEAN Connected);

/*
 * Parse a virtio-snd eventq completion buffer and update jack state if it
 * contains a supported JACK event.
 *
 * Returns TRUE only if a supported JACK event was decoded *and* it changed the
 * stored connection state.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
BOOLEAN VirtIoSndJackStateProcessEventqBuffer(
    _Inout_ PVIRTIOSND_JACK_STATE State,
    _In_reads_bytes_(UsedLen) const VOID* Buffer,
    _In_ UINT32 UsedLen,
    _Out_opt_ ULONG* OutJackId,
    _Out_opt_ BOOLEAN* OutConnected);

/*
 * Query current connection state for a jack ID. Unknown IDs return TRUE.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
BOOLEAN VirtIoSndJackStateIsConnected(_In_ const VIRTIOSND_JACK_STATE* State, _In_ ULONG JackId);

#ifdef __cplusplus
} /* extern "C" */
#endif
