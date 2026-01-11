#pragma once

#include <ntddk.h>

#include "portcls_compat.h"

NTSTATUS
VirtIoSndMiniportWaveRT_Create(_Outptr_result_maybenull_ PUNKNOWN *OutUnknown);
