#ifndef VIRTIO_PCI_CAP_PARSER_H_
#define VIRTIO_PCI_CAP_PARSER_H_

#include <stddef.h>

/*
 * WDK7 / older MSVC toolchains don't always provide <stdint.h>. Provide a small
 * fallback so the portable capability parser can be built for Windows 7.
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

#define VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT 6u

/* Standard PCI configuration space offsets (type 0 header). */
#define VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET 0x06u
#define VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET 0x34u

/* PCI Status register bits. */
#define VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST (1u << 4)

/* Standard PCI capability IDs. */
#define VIRTIO_PCI_CAP_PARSER_PCI_CAP_ID_VNDR 0x09u

/* virtio_pci_cap cfg_type values (Virtio 1.0+ modern PCI transport). */
#define VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON 1u
#define VIRTIO_PCI_CAP_PARSER_CFG_TYPE_NOTIFY 2u
#define VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR 3u
#define VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE 4u

typedef struct virtio_pci_cap_region {
    uint8_t bar;
    uint8_t id;
    uint8_t cap_len;
    uint8_t cap_offset;
    uint32_t offset;
    uint32_t length;
    uint64_t addr;
} virtio_pci_cap_region_t;

typedef struct virtio_pci_parsed_caps {
    virtio_pci_cap_region_t common_cfg;
    virtio_pci_cap_region_t notify_cfg;
    virtio_pci_cap_region_t isr_cfg;
    virtio_pci_cap_region_t device_cfg;
    uint32_t notify_off_multiplier;
} virtio_pci_parsed_caps_t;

typedef enum virtio_pci_cap_parse_result {
    VIRTIO_PCI_CAP_PARSE_OK = 0,
    VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT,
    VIRTIO_PCI_CAP_PARSE_ERR_CFG_SPACE_TOO_SMALL,
    VIRTIO_PCI_CAP_PARSE_ERR_NO_CAP_LIST,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_OUT_OF_RANGE,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_UNALIGNED,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_HEADER_TRUNCATED,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_NEXT_UNALIGNED,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_NEXT_OUT_OF_RANGE,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_LIST_LOOP,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_LEN_TOO_SMALL,
    VIRTIO_PCI_CAP_PARSE_ERR_NOTIFY_CAP_LEN_TOO_SMALL,
    VIRTIO_PCI_CAP_PARSE_ERR_CAP_TRUNCATED,
    VIRTIO_PCI_CAP_PARSE_ERR_BAR_INDEX_OUT_OF_RANGE,
    VIRTIO_PCI_CAP_PARSE_ERR_BAR_ADDRESS_MISSING,
    VIRTIO_PCI_CAP_PARSE_ERR_DUPLICATE_CFG_TYPE,
    VIRTIO_PCI_CAP_PARSE_ERR_MISSING_COMMON_CFG,
    VIRTIO_PCI_CAP_PARSE_ERR_MISSING_NOTIFY_CFG,
    VIRTIO_PCI_CAP_PARSE_ERR_MISSING_ISR_CFG,
    VIRTIO_PCI_CAP_PARSE_ERR_MISSING_DEVICE_CFG,
} virtio_pci_cap_parse_result_t;

virtio_pci_cap_parse_result_t virtio_pci_cap_parse(
    const uint8_t *cfg_space,
    size_t cfg_space_len,
    const uint64_t bar_addrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT],
    virtio_pci_parsed_caps_t *out_caps);

const char *virtio_pci_cap_parse_result_str(virtio_pci_cap_parse_result_t result);

#ifdef __cplusplus
}
#endif

#endif /* VIRTIO_PCI_CAP_PARSER_H_ */
