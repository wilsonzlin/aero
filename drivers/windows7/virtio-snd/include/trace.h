/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

// Debug logging is compiled out of free builds. Enable a checked build (DBG=1)
// to get DbgPrint output.
#if DBG
#define VIRTIOSND_TRACE(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "virtiosnd: " __VA_ARGS__)
#define VIRTIOSND_TRACE_ERROR(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL, "virtiosnd: " __VA_ARGS__)
#else
#define VIRTIOSND_TRACE(...) ((void)0)
#define VIRTIOSND_TRACE_ERROR(...) ((void)0)
#endif
