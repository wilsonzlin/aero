/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#if !defined(_POSIX_C_SOURCE)
/* For clock_gettime/nanosleep declarations when compiling as strict C99. */
#define _POSIX_C_SOURCE 200809L
#endif

/*
 * The host tests build a subset of the virtio-snd driver in user mode, and need
 * a minimal surface area of WDK headers.
 *
 * Keep a single authoritative WDK shim (drivers/windows7/virtio-snd/tests/ntddk.h)
 * and have the host tests include it through this wrapper so `#include <ntddk.h>`
 * continues to work with the existing include paths.
 */
#include "../../ntddk.h"

