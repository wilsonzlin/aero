/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd.h"

/*
 * INTx integration for virtio-snd.
 *
 * The driver uses the shared WDM INTx helper in:
 *   drivers/windows7/virtio/common/virtio_pci_intx_wdm.*
 *
 * That module implements the contract-required ISR read-to-ack semantics and
 * coalesces interrupts into a DPC callback.
 */

#ifdef __cplusplus
extern "C" {
#endif

_Must_inspect_result_ NTSTATUS
VirtIoSndIntxCaptureResources(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx, _In_opt_ PCM_RESOURCE_LIST TranslatedResources);

VOID VirtIoSndIntxInitialize(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

_Must_inspect_result_ NTSTATUS VirtIoSndIntxConnect(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);
VOID VirtIoSndIntxDisconnect(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

#ifdef __cplusplus
} /* extern "C" */
#endif
