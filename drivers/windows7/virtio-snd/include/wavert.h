#pragma once

#include <ntddk.h>

#include "portcls_compat.h"

typedef struct _VIRTIOSND_DEVICE_EXTENSION VIRTIOSND_DEVICE_EXTENSION, *PVIRTIOSND_DEVICE_EXTENSION;

NTSTATUS
VirtIoSndMiniportWaveRT_Create(_In_opt_ PVIRTIOSND_DEVICE_EXTENSION Dx, _Outptr_result_maybenull_ PUNKNOWN *OutUnknown);
