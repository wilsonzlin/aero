/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd.h"

/*
 * Virtio-pci ISR status bits (read-to-ack).
 *
 * Contract v1 requires INTx and uses the standard virtio ISR semantics:
 *  - bit 0: at least one virtqueue has used-ring entries ready.
 *  - bit 1: device-specific config change.
 */
#define VIRTIOSND_ISR_QUEUE  0x01u
#define VIRTIOSND_ISR_CONFIG 0x02u

#ifdef __cplusplus
extern "C" {
#endif

_Must_inspect_result_ NTSTATUS VirtIoSndIntxCaptureResources(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx,
                                                              _In_opt_ PCM_RESOURCE_LIST TranslatedResources);

VOID VirtIoSndIntxInitialize(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

_Must_inspect_result_ NTSTATUS VirtIoSndIntxConnect(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);
VOID VirtIoSndIntxDisconnect(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

BOOLEAN VirtIoSndIntxIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext);

VOID VirtIoSndIntxDpc(_In_ PKDPC Dpc,
                      _In_opt_ PVOID DeferredContext,
                      _In_opt_ PVOID SystemArgument1,
                      _In_opt_ PVOID SystemArgument2);

#ifdef __cplusplus
} /* extern "C" */
#endif
