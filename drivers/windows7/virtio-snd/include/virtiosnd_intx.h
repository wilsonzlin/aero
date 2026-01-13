/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd.h"

/*
 * Interrupt integration for virtio-snd (WDM).
 *
 * - Prefer message-signaled interrupts (MSI/MSI-X) when the PnP resource list
 *   contains CM_RESOURCE_INTERRUPT_MESSAGE.
 * - Fall back to legacy line-based INTx (contract v1 default).
 *
 * INTx uses the shared WDM helper in:
 *   drivers/windows7/virtio/common/virtio_pci_intx_wdm.*
 *
 * That helper implements the contract-required ISR read-to-ack semantics and
 * coalesces interrupts into a DPC callback.
 */

#ifdef __cplusplus
extern "C" {
#endif

_Must_inspect_result_ NTSTATUS
VirtIoSndInterruptCaptureResources(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx, _In_opt_ PCM_RESOURCE_LIST TranslatedResources);

VOID VirtIoSndInterruptInitialize(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/*
 * Connect message-signaled interrupts (MSI/MSI-X) if available in the resource
 * list.
 *
 * This uses IoConnectInterruptEx(CONNECT_MESSAGE_BASED) and fills the
 * MessageInterrupt* fields on the device extension.
 */
_Must_inspect_result_ NTSTATUS VirtIoSndInterruptConnectMessage(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/*
 * Connect legacy INTx. Intended to be called when MSI/MSI-X was not connected
 * (or failed) and the translated resources contain a line-based interrupt.
 */
_Must_inspect_result_ NTSTATUS VirtIoSndInterruptConnectIntx(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/* Disconnect whichever interrupt mode is currently connected. */
VOID VirtIoSndInterruptDisconnect(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/* Return the maximum "DPC in flight" count across INTx and MSI/MSI-X. */
LONG VirtIoSndInterruptGetDpcInFlight(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

/*
 * Best-effort: clear virtio MSI-X vector routing (msix_config/queue_msix_vector)
 * before reset when MSI/MSI-X is active.
 */
VOID VirtIoSndInterruptDisableDeviceVectors(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

#ifdef __cplusplus
} /* extern "C" */
#endif
