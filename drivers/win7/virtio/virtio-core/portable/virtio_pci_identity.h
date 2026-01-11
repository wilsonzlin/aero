#ifndef VIRTIO_PCI_IDENTITY_H_
#define VIRTIO_PCI_IDENTITY_H_

#include <stddef.h>

/*
 * WDK7 / older MSVC toolchains don't always provide <stdint.h>. Provide a small
 * fallback so this header can be built for Windows 7.
 */
#if defined(_MSC_VER) && (_MSC_VER < 1600)
typedef signed __int8 int8_t;
typedef unsigned __int8 uint8_t;
typedef signed __int16 int16_t;
typedef unsigned __int16 uint16_t;
typedef signed __int32 int32_t;
typedef unsigned __int32 uint32_t;
typedef unsigned __int64 uint64_t;
#else
#include <stdint.h>
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Standard PCI config space offsets (type 0 header). */
#define VIRTIO_PCI_IDENTITY_PCI_VENDOR_ID_OFFSET 0x00u
#define VIRTIO_PCI_IDENTITY_PCI_DEVICE_ID_OFFSET 0x02u
#define VIRTIO_PCI_IDENTITY_PCI_REVISION_ID_OFFSET 0x08u
#define VIRTIO_PCI_IDENTITY_PCI_SUBSYSTEM_VENDOR_ID_OFFSET 0x2cu
#define VIRTIO_PCI_IDENTITY_PCI_SUBSYSTEM_ID_OFFSET 0x2eu

#define VIRTIO_PCI_IDENTITY_VENDOR_ID_VIRTIO 0x1af4u

/*
 * Virtio 1.0+ "modern-only" virtio-pci device ID space:
 *   device_id = 0x1040 + virtio_device_id
 *
 * Contract v1 requires drivers to bind only to this modern ID space.
 */
#define VIRTIO_PCI_IDENTITY_DEVICE_ID_MODERN_BASE 0x1040u

/* Aero Windows 7 virtio contract v1 major version (encoded in PCI Revision ID). */
#define VIRTIO_PCI_IDENTITY_AERO_CONTRACT_V1_REVISION_ID 0x01u

typedef struct virtio_pci_identity {
    uint16_t vendor_id;
    uint16_t device_id;
    uint8_t revision_id;

    /*
     * Optional (may be 0 if cfg_space_len is too small to read them).
     * Aero devices set these to 0x1AF4 and an Aero-specific subsystem id.
     */
    uint16_t subsystem_vendor_id;
    uint16_t subsystem_id;
} virtio_pci_identity_t;

typedef enum virtio_pci_identity_result {
    VIRTIO_PCI_IDENTITY_OK = 0,
    VIRTIO_PCI_IDENTITY_ERR_BAD_ARGUMENT,
    VIRTIO_PCI_IDENTITY_ERR_CFG_SPACE_TOO_SMALL,
    VIRTIO_PCI_IDENTITY_ERR_VENDOR_MISMATCH,
    VIRTIO_PCI_IDENTITY_ERR_DEVICE_ID_NOT_MODERN,
    VIRTIO_PCI_IDENTITY_ERR_DEVICE_ID_NOT_ALLOWED,
    VIRTIO_PCI_IDENTITY_ERR_REVISION_MISMATCH,
} virtio_pci_identity_result_t;

virtio_pci_identity_result_t virtio_pci_identity_parse(
    const uint8_t *cfg_space,
    size_t cfg_space_len,
    virtio_pci_identity_t *out_identity);

/*
 * Validates AERO-W7-VIRTIO contract v1 identity requirements:
 *   - PCI Vendor ID == 0x1AF4
 *   - PCI Revision ID == 0x01
 *   - PCI Device ID in the modern-only ID space (>= 0x1040)
 *   - (optional) Device ID is in the allowed list for the caller/driver.
 *
 * If out_identity is non-NULL, it is populated on both success and failure.
 */
virtio_pci_identity_result_t virtio_pci_identity_validate_aero_contract_v1(
    const uint8_t *cfg_space,
    size_t cfg_space_len,
    const uint16_t *allowed_device_ids,
    size_t allowed_device_id_count,
    virtio_pci_identity_t *out_identity);

const char *virtio_pci_identity_result_str(virtio_pci_identity_result_t result);

#ifdef __cplusplus
}
#endif

#endif /* VIRTIO_PCI_IDENTITY_H_ */
