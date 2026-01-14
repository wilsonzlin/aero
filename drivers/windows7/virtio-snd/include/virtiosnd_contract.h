/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd_queue.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Pure helper: validate the virtio-snd DEVICE_CFG values required by the
 * Aero Windows 7 virtio-snd contract v1 (sec 3.4.5).
 */
_Must_inspect_result_ BOOLEAN VirtIoSndValidateDeviceCfgValues(_In_ ULONG Jacks, _In_ ULONG Streams, _In_ ULONG Chmaps);

/*
 * Pure helper: return the expected virtqueue size for the given contract-v1
 * queue index (sec 3.4.2).
 *
 * Returns 0 for unknown indices.
 */
_Must_inspect_result_ USHORT VirtIoSndExpectedQueueSize(_In_ USHORT QueueIndex);

#ifdef __cplusplus
} /* extern "C" */
#endif
