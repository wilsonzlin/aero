/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "portcls_compat.h"

NTSTATUS
VirtIoSndMiniportWaveRT_Create(_In_ struct _VIRTIOSND_DEVICE_EXTENSION *Dx,
                               _Outptr_result_maybenull_ PUNKNOWN *OutUnknown);
