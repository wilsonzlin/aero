#include "virtio_pci_aero_layout.h"

enum {
    VIRTIO_PCI_AERO_BAR0_MIN_LEN = 0x4000u,

    VIRTIO_PCI_AERO_COMMON_OFF = 0x0000u,
    VIRTIO_PCI_AERO_COMMON_MIN_LEN = 0x0100u,

    VIRTIO_PCI_AERO_NOTIFY_OFF = 0x1000u,
    VIRTIO_PCI_AERO_NOTIFY_MIN_LEN = 0x0100u,
    VIRTIO_PCI_AERO_NOTIFY_MULT = 4u,

    VIRTIO_PCI_AERO_ISR_OFF = 0x2000u,
    VIRTIO_PCI_AERO_ISR_MIN_LEN = 0x0020u,

    VIRTIO_PCI_AERO_DEVICE_OFF = 0x3000u,
    VIRTIO_PCI_AERO_DEVICE_MIN_LEN = 0x0100u,
};

static int virtio_pci_aero_cap_matches(
    const virtio_pci_cap_region_t *cap,
    uint8_t expected_bar,
    uint32_t expected_offset,
    uint32_t min_length) {
    if (cap == NULL) {
        return 0;
    }

    if (cap->bar != expected_bar) {
        return 0;
    }

    if (cap->offset != expected_offset) {
        return 0;
    }

    if (cap->length < min_length) {
        return 0;
    }

    return 1;
}

virtio_pci_aero_layout_validate_result_t virtio_pci_validate_aero_pci_layout(
    const virtio_pci_parsed_caps_t *caps,
    const virtio_pci_bar_info_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT],
    virtio_pci_layout_policy_t policy) {
    if (caps == NULL || bars == NULL) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAD_ARGUMENT;
    }

    if (policy == VIRTIO_PCI_LAYOUT_POLICY_PERMISSIVE) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK;
    }

    if (policy != VIRTIO_PCI_LAYOUT_POLICY_AERO_STRICT) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAD_ARGUMENT;
    }

    if (bars[0].present == 0) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_MISSING;
    }

    if (bars[0].is_memory == 0) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_NOT_MMIO;
    }

    if (bars[0].length < (uint64_t)VIRTIO_PCI_AERO_BAR0_MIN_LEN) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_TOO_SMALL;
    }

    if (!virtio_pci_aero_cap_matches(&caps->common_cfg, 0, VIRTIO_PCI_AERO_COMMON_OFF, VIRTIO_PCI_AERO_COMMON_MIN_LEN)) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_COMMON_MISMATCH;
    }

    if (!virtio_pci_aero_cap_matches(&caps->notify_cfg, 0, VIRTIO_PCI_AERO_NOTIFY_OFF, VIRTIO_PCI_AERO_NOTIFY_MIN_LEN)) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_NOTIFY_MISMATCH;
    }

    if (!virtio_pci_aero_cap_matches(&caps->isr_cfg, 0, VIRTIO_PCI_AERO_ISR_OFF, VIRTIO_PCI_AERO_ISR_MIN_LEN)) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_ISR_MISMATCH;
    }

    if (!virtio_pci_aero_cap_matches(
            &caps->device_cfg, 0, VIRTIO_PCI_AERO_DEVICE_OFF, VIRTIO_PCI_AERO_DEVICE_MIN_LEN)) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_DEVICE_MISMATCH;
    }

    if (caps->notify_off_multiplier != VIRTIO_PCI_AERO_NOTIFY_MULT) {
        return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_NOTIFY_MULTIPLIER_MISMATCH;
    }

    return VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK;
}

const char *virtio_pci_aero_layout_validate_result_str(virtio_pci_aero_layout_validate_result_t result) {
    switch (result) {
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK:
            return "OK";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAD_ARGUMENT:
            return "BAD_ARGUMENT";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_MISSING:
            return "BAR0_MISSING";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_NOT_MMIO:
            return "BAR0_NOT_MMIO";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_BAR0_TOO_SMALL:
            return "BAR0_TOO_SMALL";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_COMMON_MISMATCH:
            return "COMMON_MISMATCH";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_NOTIFY_MISMATCH:
            return "NOTIFY_MISMATCH";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_ISR_MISMATCH:
            return "ISR_MISMATCH";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_DEVICE_MISMATCH:
            return "DEVICE_MISMATCH";
        case VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_NOTIFY_MULTIPLIER_MISMATCH:
            return "NOTIFY_MULTIPLIER_MISMATCH";
        default:
            return "UNKNOWN";
    }
}

