/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "portcls_compat.h"

/*
 * Topology jack state is updated best-effort from virtio-snd eventq JACK events.
 *
 * The current topology miniport exposes two endpoint jacks:
 *  - Jack 0: speaker/render endpoint
 *  - Jack 1: microphone/capture endpoint
 *
 * If the device never sends jack events, the driver defaults both jacks to
 * "connected" to preserve existing behavior.
 */
#define VIRTIOSND_JACK_ID_SPEAKER     0u
#define VIRTIOSND_JACK_ID_MICROPHONE  1u
#define VIRTIOSND_JACK_ID_COUNT       2u

/*
 * Initialize global topology helper state (spinlocks, default jack state).
 *
 * Called from DriverEntry so that event callbacks can safely use shared state.
 */
VOID VirtIoSndTopology_Initialize(VOID);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtIoSndTopology_ResetJackState(VOID);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtIoSndTopology_UpdateJackState(_In_ ULONG JackId, _In_ BOOLEAN IsConnected);

_IRQL_requires_max_(DISPATCH_LEVEL)
BOOLEAN VirtIoSndTopology_IsJackConnected(_In_ ULONG JackId);

NTSTATUS
VirtIoSndMiniportTopology_Create(_Outptr_result_maybenull_ PUNKNOWN *OutUnknown);
