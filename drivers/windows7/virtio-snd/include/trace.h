/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

/*
 * WinDDK 7600 headers do not always provide the NT_ASSERT() macro.
 * Use ASSERT() as a compatible fallback.
 */
#ifndef NT_ASSERT
#define NT_ASSERT(_expr) ASSERT(_expr)
#endif

/*
 * Logging policy:
 *   - VIRTIOSND_TRACE: verbose/info tracing, compiled out of free builds unless DBG=1.
 *   - VIRTIOSND_TRACE_ERROR: always enabled by default, even in free builds, so
 *     bring-up failures (Code 10, etc.) are diagnosable without a checked build.
 *
 * Define VIRTIOSND_ENABLE_ERROR_LOGS=0 to compile out error logs if needed.
 */
#ifndef VIRTIOSND_ENABLE_ERROR_LOGS
#define VIRTIOSND_ENABLE_ERROR_LOGS 1
#endif

#if DBG
#define VIRTIOSND_TRACE(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "virtiosnd: " __VA_ARGS__)
#else
#define VIRTIOSND_TRACE(...) ((void)0)
#endif

#if VIRTIOSND_ENABLE_ERROR_LOGS
#define VIRTIOSND_TRACE_ERROR(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL, "virtiosnd: " __VA_ARGS__)
#else
#define VIRTIOSND_TRACE_ERROR(...) ((void)0)
#endif
