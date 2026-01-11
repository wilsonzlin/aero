/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "portcls_compat.h"

NTSTATUS
VirtIoSndMiniportTopology_Create(_Outptr_result_maybenull_ PUNKNOWN *OutUnknown);
