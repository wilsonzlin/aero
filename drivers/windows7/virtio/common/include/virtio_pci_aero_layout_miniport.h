/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * AERO-W7-VIRTIO contract v1 fixed BAR0 MMIO layout validation for miniports.
 *
 * This helper is shared by the Windows 7 virtio-blk (StorPort) and virtio-net
 * (NDIS) miniport drivers. The contract requires a fixed virtio-pci modern MMIO
 * layout within BAR0.
 */

#pragma once

#include "virtio_pci_modern_miniport.h"

/*
 * Build-time switch: strict vs permissive BAR0 layout validation.
 *
 * When enabled (default), AeroVirtioValidateContractV1Bar0Layout enforces the
 * fixed offsets from docs/windows7-virtio-driver-contract.md (§1.4):
 *   - common @ 0x0000
 *   - notify @ 0x1000
 *   - isr    @ 0x2000
 *   - device @ 0x3000
 *
 * When disabled, the helper still validates:
 *   - BAR0 length is large enough for the contract (>= 0x4000)
 *   - required virtio capabilities are present and contained within BAR0
 *   - notify_off_multiplier == 4
 *
 * but does not require the fixed offsets.
 *
 * This is intended as a compatibility/debug knob for developers (e.g. when
 * comparing behavior against QEMU); contract-v1 conformance remains strict by
 * default.
 */
#ifndef AERO_VIRTIO_MINIPORT_ENFORCE_FIXED_LAYOUT
#define AERO_VIRTIO_MINIPORT_ENFORCE_FIXED_LAYOUT 1
#endif

_Must_inspect_result_
BOOLEAN
AeroVirtioValidateContractV1Bar0Layout(_In_ const VIRTIO_PCI_DEVICE *Dev);
