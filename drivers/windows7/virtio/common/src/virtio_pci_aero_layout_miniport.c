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

static BOOLEAN
AeroVirtioValidateBar0Region(_In_ const VIRTIO_PCI_DEVICE *Dev,
                             _In_ ULONG Offset,
                             _In_ ULONG Length,
                             _In_ ULONG RequiredMinLength)
{
    ULONGLONG end;

    if (Dev == NULL) {
        return FALSE;
    }

    if (Length < RequiredMinLength) {
        return FALSE;
    }

    end = (ULONGLONG)Offset + (ULONGLONG)Length;
    if (end < Offset) {
        return FALSE;
    }

    if (end > (ULONGLONG)Dev->Bar0Length) {
        return FALSE;
    }

    return TRUE;
}

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

#if AERO_VIRTIO_MINIPORT_ENFORCE_FIXED_LAYOUT
    /*
     * Strict mode: enforce the contract v1 fixed offsets and minimum region
     * sizes.
     */
    if (Dev->CommonCfgOffset != AERO_VIRTIO_CONTRACT_V1_COMMON_OFFSET ||
        !AeroVirtioValidateBar0Region(Dev, Dev->CommonCfgOffset, Dev->CommonCfgLength, AERO_VIRTIO_CONTRACT_V1_COMMON_MIN_LEN)) {
        return FALSE;
    }

    if (Dev->NotifyOffset != AERO_VIRTIO_CONTRACT_V1_NOTIFY_OFFSET ||
        !AeroVirtioValidateBar0Region(Dev, Dev->NotifyOffset, Dev->NotifyLength, AERO_VIRTIO_CONTRACT_V1_NOTIFY_MIN_LEN)) {
        return FALSE;
    }

    if (Dev->IsrOffset != AERO_VIRTIO_CONTRACT_V1_ISR_OFFSET ||
        !AeroVirtioValidateBar0Region(Dev, Dev->IsrOffset, Dev->IsrLength, AERO_VIRTIO_CONTRACT_V1_ISR_MIN_LEN)) {
        return FALSE;
    }

    if (Dev->DeviceCfgOffset != AERO_VIRTIO_CONTRACT_V1_DEVICE_OFFSET ||
        !AeroVirtioValidateBar0Region(Dev, Dev->DeviceCfgOffset, Dev->DeviceCfgLength, AERO_VIRTIO_CONTRACT_V1_DEVICE_MIN_LEN)) {
        return FALSE;
    }
#else
    /*
     * Permissive mode: keep validating that all required capabilities exist and
     * are contained within BAR0, but do not require the fixed offsets.
     *
     * The per-capability minimum lengths below are the minimum the miniport
     * transport helpers require for safe access (they match the validation in
     * VirtioPciModernMiniportInit()).
     */
    if (!AeroVirtioValidateBar0Region(Dev, Dev->CommonCfgOffset, Dev->CommonCfgLength, (ULONG)sizeof(virtio_pci_common_cfg))) {
        return FALSE;
    }

    if (!AeroVirtioValidateBar0Region(Dev, Dev->NotifyOffset, Dev->NotifyLength, (ULONG)sizeof(UINT16))) {
        return FALSE;
    }

    if (!AeroVirtioValidateBar0Region(Dev, Dev->IsrOffset, Dev->IsrLength, 1)) {
        return FALSE;
    }

    if (!AeroVirtioValidateBar0Region(Dev, Dev->DeviceCfgOffset, Dev->DeviceCfgLength, 1)) {
        return FALSE;
    }
#endif

    return TRUE;
}
