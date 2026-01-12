/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtio_pci_aero_layout_miniport.h"

enum {
    AERO_VIRTIO_CONTRACT_V1_BAR0_MIN_LEN = 0x4000u,

    AERO_VIRTIO_CONTRACT_V1_COMMON_OFFSET = 0x0000u,
    AERO_VIRTIO_CONTRACT_V1_COMMON_MIN_LEN = 0x0100u,

    AERO_VIRTIO_CONTRACT_V1_NOTIFY_OFFSET = 0x1000u,
    AERO_VIRTIO_CONTRACT_V1_NOTIFY_MIN_LEN = 0x0100u,
    AERO_VIRTIO_CONTRACT_V1_NOTIFY_MULT = 4u,

    AERO_VIRTIO_CONTRACT_V1_ISR_OFFSET = 0x2000u,
    AERO_VIRTIO_CONTRACT_V1_ISR_MIN_LEN = 0x0020u,

    AERO_VIRTIO_CONTRACT_V1_DEVICE_OFFSET = 0x3000u,
    AERO_VIRTIO_CONTRACT_V1_DEVICE_MIN_LEN = 0x0100u,
};

_Must_inspect_result_
BOOLEAN
AeroVirtioValidateContractV1Bar0Layout(_In_ const VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL) {
        return FALSE;
    }

    /*
     * Contract v1 fixed BAR0 layout + notify multiplier.
     */
    if (Dev->Bar0Length < AERO_VIRTIO_CONTRACT_V1_BAR0_MIN_LEN) {
        return FALSE;
    }

    if (Dev->NotifyOffMultiplier != AERO_VIRTIO_CONTRACT_V1_NOTIFY_MULT) {
        return FALSE;
    }

    if (Dev->CommonCfgOffset != AERO_VIRTIO_CONTRACT_V1_COMMON_OFFSET || Dev->CommonCfgLength < AERO_VIRTIO_CONTRACT_V1_COMMON_MIN_LEN) {
        return FALSE;
    }

    if (Dev->NotifyOffset != AERO_VIRTIO_CONTRACT_V1_NOTIFY_OFFSET || Dev->NotifyLength < AERO_VIRTIO_CONTRACT_V1_NOTIFY_MIN_LEN) {
        return FALSE;
    }

    if (Dev->IsrOffset != AERO_VIRTIO_CONTRACT_V1_ISR_OFFSET || Dev->IsrLength < AERO_VIRTIO_CONTRACT_V1_ISR_MIN_LEN) {
        return FALSE;
    }

    if (Dev->DeviceCfgOffset != AERO_VIRTIO_CONTRACT_V1_DEVICE_OFFSET || Dev->DeviceCfgLength < AERO_VIRTIO_CONTRACT_V1_DEVICE_MIN_LEN) {
        return FALSE;
    }

    return TRUE;
}
