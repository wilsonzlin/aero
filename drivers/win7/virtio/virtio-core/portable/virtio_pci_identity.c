#include "virtio_pci_identity.h"

static uint16_t read_le16(const uint8_t *p)
{
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}

virtio_pci_identity_result_t virtio_pci_identity_parse(
    const uint8_t *cfg_space,
    size_t cfg_space_len,
    virtio_pci_identity_t *out_identity)
{
    if (cfg_space == NULL || out_identity == NULL) {
        return VIRTIO_PCI_IDENTITY_ERR_BAD_ARGUMENT;
    }

    if (cfg_space_len <= VIRTIO_PCI_IDENTITY_PCI_REVISION_ID_OFFSET) {
        return VIRTIO_PCI_IDENTITY_ERR_CFG_SPACE_TOO_SMALL;
    }

    out_identity->vendor_id = read_le16(cfg_space + VIRTIO_PCI_IDENTITY_PCI_VENDOR_ID_OFFSET);
    out_identity->device_id = read_le16(cfg_space + VIRTIO_PCI_IDENTITY_PCI_DEVICE_ID_OFFSET);
    out_identity->revision_id = cfg_space[VIRTIO_PCI_IDENTITY_PCI_REVISION_ID_OFFSET];

    out_identity->subsystem_vendor_id = 0;
    out_identity->subsystem_id = 0;

    if (cfg_space_len >= (VIRTIO_PCI_IDENTITY_PCI_SUBSYSTEM_ID_OFFSET + 2)) {
        out_identity->subsystem_vendor_id =
            read_le16(cfg_space + VIRTIO_PCI_IDENTITY_PCI_SUBSYSTEM_VENDOR_ID_OFFSET);
        out_identity->subsystem_id = read_le16(cfg_space + VIRTIO_PCI_IDENTITY_PCI_SUBSYSTEM_ID_OFFSET);
    }

    return VIRTIO_PCI_IDENTITY_OK;
}

virtio_pci_identity_result_t virtio_pci_identity_validate_aero_contract_v1(
    const uint8_t *cfg_space,
    size_t cfg_space_len,
    const uint16_t *allowed_device_ids,
    size_t allowed_device_id_count,
    virtio_pci_identity_t *out_identity)
{
    virtio_pci_identity_t tmp;
    virtio_pci_identity_t *id;
    virtio_pci_identity_result_t res;

    if (cfg_space == NULL) {
        return VIRTIO_PCI_IDENTITY_ERR_BAD_ARGUMENT;
    }

    id = (out_identity != NULL) ? out_identity : &tmp;

    res = virtio_pci_identity_parse(cfg_space, cfg_space_len, id);
    if (res != VIRTIO_PCI_IDENTITY_OK) {
        return res;
    }

    if (id->vendor_id != (uint16_t)VIRTIO_PCI_IDENTITY_VENDOR_ID_VIRTIO) {
        return VIRTIO_PCI_IDENTITY_ERR_VENDOR_MISMATCH;
    }

    if (id->revision_id != (uint8_t)VIRTIO_PCI_IDENTITY_AERO_CONTRACT_V1_REVISION_ID) {
        return VIRTIO_PCI_IDENTITY_ERR_REVISION_MISMATCH;
    }

    if (id->device_id < (uint16_t)VIRTIO_PCI_IDENTITY_DEVICE_ID_MODERN_BASE) {
        return VIRTIO_PCI_IDENTITY_ERR_DEVICE_ID_NOT_MODERN;
    }

    if (allowed_device_ids != NULL && allowed_device_id_count != 0) {
        size_t i;
        for (i = 0; i < allowed_device_id_count; ++i) {
            if (id->device_id == allowed_device_ids[i]) {
                return VIRTIO_PCI_IDENTITY_OK;
            }
        }
        return VIRTIO_PCI_IDENTITY_ERR_DEVICE_ID_NOT_ALLOWED;
    }

    return VIRTIO_PCI_IDENTITY_OK;
}

const char *virtio_pci_identity_result_str(virtio_pci_identity_result_t result)
{
    switch (result) {
    case VIRTIO_PCI_IDENTITY_OK:
        return "OK";
    case VIRTIO_PCI_IDENTITY_ERR_BAD_ARGUMENT:
        return "BAD_ARGUMENT";
    case VIRTIO_PCI_IDENTITY_ERR_CFG_SPACE_TOO_SMALL:
        return "CFG_SPACE_TOO_SMALL";
    case VIRTIO_PCI_IDENTITY_ERR_VENDOR_MISMATCH:
        return "VENDOR_MISMATCH";
    case VIRTIO_PCI_IDENTITY_ERR_DEVICE_ID_NOT_MODERN:
        return "DEVICE_ID_NOT_MODERN";
    case VIRTIO_PCI_IDENTITY_ERR_DEVICE_ID_NOT_ALLOWED:
        return "DEVICE_ID_NOT_ALLOWED";
    case VIRTIO_PCI_IDENTITY_ERR_REVISION_MISMATCH:
        return "REVISION_MISMATCH";
    default:
        return "UNKNOWN";
    }
}

