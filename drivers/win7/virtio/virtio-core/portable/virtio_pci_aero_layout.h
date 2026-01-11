#ifndef VIRTIO_PCI_AERO_LAYOUT_H_
#define VIRTIO_PCI_AERO_LAYOUT_H_

#include <stddef.h>

#include "virtio_pci_cap_parser.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Aero Windows 7 virtio device contract v1 fixes a single virtio-pci modern MMIO
 * layout within BAR0:
 *   - BAR0: MMIO, size >= 0x4000
 *   - COMMON: bar=0 off=0x0000 len>=0x0100
 *   - NOTIFY: bar=0 off=0x1000 len>=0x0100, notify_off_multiplier == 4
 *   - ISR:    bar=0 off=0x2000 len>=0x0020
 *   - DEVICE: bar=0 off=0x3000 len>=0x0100
 *
 * The portable virtio capability parser intentionally supports arbitrary modern
 * virtio layouts (e.g. QEMU's multi-BAR placement). This file provides an
 * optional strict validation layer so the Windows transport can operate in:
 *   - permissive mode (default): accept any valid modern placement
 *   - strict mode: enforce the Aero fixed layout and fail init on mismatch
 */

typedef enum virtio_pci_layout_policy {
    VIRTIO_PCI_LAYOUT_POLICY_PERMISSIVE = 0,
    VIRTIO_PCI_LAYOUT_POLICY_AERO_STRICT = 1,
} virtio_pci_layout_policy_t;

typedef struct virtio_pci_bar_info {
    /*
     * Whether the BAR exists and has a known length (e.g. a matched resource).
     * This is transport-specific; in the Windows KMDF transport we treat a BAR
     * as "present" only after it has been matched/mapped.
     */
    uint8_t present;

    /* Non-zero if this BAR is MMIO (memory space), zero if I/O space. */
    uint8_t is_memory;

    /* BAR size in bytes. Only meaningful when present != 0. */
    uint64_t length;
} virtio_pci_bar_info_t;

typedef enum virtio_pci_aero_layout_validate_result {
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK = 0,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAD_ARGUMENT,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_MISSING,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_NOT_MMIO,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_TOO_SMALL,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_COMMON_MISMATCH,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_NOTIFY_MISMATCH,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_ISR_MISMATCH,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_DEVICE_MISMATCH,
    VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_NOTIFY_MULTIPLIER_MISMATCH,
} virtio_pci_aero_layout_validate_result_t;

virtio_pci_aero_layout_validate_result_t virtio_pci_validate_aero_pci_layout(
    const virtio_pci_parsed_caps_t *caps,
    const virtio_pci_bar_info_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT],
    virtio_pci_layout_policy_t policy);

const char *virtio_pci_aero_layout_validate_result_str(virtio_pci_aero_layout_validate_result_t result);

#ifdef __cplusplus
}
#endif

#endif /* VIRTIO_PCI_AERO_LAYOUT_H_ */

