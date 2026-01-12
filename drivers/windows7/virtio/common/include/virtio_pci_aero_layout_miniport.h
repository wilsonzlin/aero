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

_Must_inspect_result_
BOOLEAN
AeroVirtioValidateContractV1Bar0Layout(_In_ const VIRTIO_PCI_DEVICE *Dev);
