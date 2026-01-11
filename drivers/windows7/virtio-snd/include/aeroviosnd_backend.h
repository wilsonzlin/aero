/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "backend.h"

/*
 * Backend implementation used by the PortCls/WaveRT miniport to push PCM into the
 * Aero virtio-snd device via the legacy virtio-pci I/O-port transport (transitional
 * virtio-snd / `DEV_1018` style devices).
 *
 * Note: The Aero Windows 7 virtio contract v1 (`AERO-W7-VIRTIO`) is modern-only.
 * For virtio-pci modern bring-up (PCI vendor caps + BAR0 MMIO + INTx), see:
 * `docs/windows/virtio-pci-modern-wdm.md`.
 *
 * This header intentionally forward-declares the hardware device extension to
 * avoid pulling in the full device/virtio headers from the WaveRT miniport code.
 */

typedef struct _AEROVIOSND_DEVICE_EXTENSION AEROVIOSND_DEVICE_EXTENSION, *PAEROVIOSND_DEVICE_EXTENSION;

_Must_inspect_result_ NTSTATUS VirtIoSndBackendLegacy_Create(_In_ PAEROVIOSND_DEVICE_EXTENSION Dx,
                                                            _Outptr_result_maybenull_ PVIRTIOSND_BACKEND* OutBackend);
