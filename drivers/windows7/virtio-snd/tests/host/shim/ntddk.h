/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

/*
 * The host tests build a subset of the virtio-snd driver in user mode, and need
 * a minimal surface area of WDK headers.
 *
 * Keep a single authoritative WDK shim (drivers/windows7/virtio-snd/tests/ntddk.h)
 * and have the host tests include it through this wrapper so `#include <ntddk.h>`
 * continues to work with the existing include paths.
 */
#include "../../ntddk.h"
