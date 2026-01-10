#include "virtio_pci_cap_parser.h"

enum {
    VIRTIO_PCI_CAP_PARSER_CFG_MIN_LEN = 0x40,
    VIRTIO_PCI_CAP_PARSER_VIRTIO_CAP_LEN = 16,
    VIRTIO_PCI_CAP_PARSER_VIRTIO_NOTIFY_CAP_LEN = 20,
};

static void virtio_pci_cap_parser_zero(void *ptr, size_t len) {
    uint8_t *p = (uint8_t *)ptr;
    size_t i;

    for (i = 0; i < len; ++i) {
        p[i] = 0;
    }
}

static uint32_t virtio_pci_cap_parser_read_le32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static virtio_pci_cap_parse_result_t virtio_pci_cap_parser_sanitize_cap_ptr(
    uint8_t raw_ptr,
    size_t cfg_space_len,
    uint8_t *out_ptr) {
    uint8_t ptr;

    if (out_ptr == NULL) {
        return VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT;
    }

    ptr = (uint8_t)(raw_ptr & 0xFCu);
    if (ptr == 0) {
        return VIRTIO_PCI_CAP_PARSE_ERR_NO_CAP_LIST;
    }

    if (ptr < VIRTIO_PCI_CAP_PARSER_CFG_MIN_LEN) {
        return VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_OUT_OF_RANGE;
    }

    if ((size_t)ptr >= cfg_space_len) {
        return VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_OUT_OF_RANGE;
    }

    *out_ptr = ptr;
    return VIRTIO_PCI_CAP_PARSE_OK;
}

static virtio_pci_cap_parse_result_t virtio_pci_cap_parser_sanitize_cap_next(
    uint8_t raw_next,
    size_t cfg_space_len,
    uint8_t *out_next) {
    uint8_t next;

    if (out_next == NULL) {
        return VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT;
    }

    next = (uint8_t)(raw_next & 0xFCu);
    if (next == 0) {
        *out_next = 0;
        return VIRTIO_PCI_CAP_PARSE_OK;
    }

    if (next < VIRTIO_PCI_CAP_PARSER_CFG_MIN_LEN) {
        return VIRTIO_PCI_CAP_PARSE_ERR_CAP_NEXT_OUT_OF_RANGE;
    }

    if ((size_t)next >= cfg_space_len) {
        return VIRTIO_PCI_CAP_PARSE_ERR_CAP_NEXT_OUT_OF_RANGE;
    }

    *out_next = next;
    return VIRTIO_PCI_CAP_PARSE_OK;
}

static virtio_pci_cap_parse_result_t virtio_pci_cap_parser_store_region(
    virtio_pci_cap_region_t *out,
    const uint64_t bar_addrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT],
    uint8_t bar,
    uint8_t id,
    uint8_t cap_len,
    uint8_t cap_offset,
    uint32_t offset,
    uint32_t length) {
    uint64_t base;

    if (bar >= VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT) {
        return VIRTIO_PCI_CAP_PARSE_ERR_BAR_INDEX_OUT_OF_RANGE;
    }

    base = bar_addrs[bar];
    if (base == 0) {
        return VIRTIO_PCI_CAP_PARSE_ERR_BAR_ADDRESS_MISSING;
    }

    out->bar = bar;
    out->id = id;
    out->cap_len = cap_len;
    out->cap_offset = cap_offset;
    out->offset = offset;
    out->length = length;
    out->addr = base + (uint64_t)offset;
    return VIRTIO_PCI_CAP_PARSE_OK;
}

virtio_pci_cap_parse_result_t virtio_pci_cap_parse(
    const uint8_t *cfg_space,
    size_t cfg_space_len,
    const uint64_t bar_addrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT],
    virtio_pci_parsed_caps_t *out_caps) {
    uint8_t cap_ptr;
    uint8_t visited[256];
    size_t i;
    uint8_t current;
    uint8_t found_common;
    uint8_t found_notify;
    uint8_t found_isr;
    uint8_t found_device;
    size_t caps_seen;

    if (cfg_space == NULL || bar_addrs == NULL || out_caps == NULL) {
        return VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT;
    }

    virtio_pci_cap_parser_zero(out_caps, sizeof(*out_caps));

    if (cfg_space_len < VIRTIO_PCI_CAP_PARSER_CFG_MIN_LEN) {
        return VIRTIO_PCI_CAP_PARSE_ERR_CFG_SPACE_TOO_SMALL;
    }

    {
        virtio_pci_cap_parse_result_t res = virtio_pci_cap_parser_sanitize_cap_ptr(
            cfg_space[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET], cfg_space_len, &cap_ptr);
        if (res != VIRTIO_PCI_CAP_PARSE_OK) {
            return res;
        }
    }

    for (i = 0; i < sizeof(visited); ++i) {
        visited[i] = 0;
    }

    found_common = 0;
    found_notify = 0;
    found_isr = 0;
    found_device = 0;
    caps_seen = 0;

    current = cap_ptr;
    while (current != 0) {
        uint8_t cap_id;
        uint8_t cap_next;

        if (visited[current] != 0) {
            return VIRTIO_PCI_CAP_PARSE_ERR_CAP_LIST_LOOP;
        }
        visited[current] = 1;

        if ((size_t)current + 2u > cfg_space_len) {
            return VIRTIO_PCI_CAP_PARSE_ERR_CAP_HEADER_TRUNCATED;
        }

        cap_id = cfg_space[current + 0];
        {
            virtio_pci_cap_parse_result_t res =
                virtio_pci_cap_parser_sanitize_cap_next(cfg_space[current + 1], cfg_space_len, &cap_next);
            if (res != VIRTIO_PCI_CAP_PARSE_OK) {
                return res;
            }
        }

        ++caps_seen;
        if (caps_seen > 64) {
            return VIRTIO_PCI_CAP_PARSE_ERR_CAP_LIST_LOOP;
        }

        if (cap_id == VIRTIO_PCI_CAP_PARSER_PCI_CAP_ID_VNDR) {
            uint8_t cap_len;
            uint8_t cfg_type;
            uint8_t bar;
            uint32_t offset;
            uint32_t length;

            if ((size_t)current + 4u > cfg_space_len) {
                return VIRTIO_PCI_CAP_PARSE_ERR_CAP_HEADER_TRUNCATED;
            }

            cap_len = cfg_space[current + 2];
            cfg_type = cfg_space[current + 3];

            if (cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON ||
                cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_NOTIFY ||
                cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR ||
                cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE) {
                uint8_t id;

                if (cap_len < VIRTIO_PCI_CAP_PARSER_VIRTIO_CAP_LEN) {
                    return VIRTIO_PCI_CAP_PARSE_ERR_CAP_LEN_TOO_SMALL;
                }

                if (cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_NOTIFY &&
                    cap_len < VIRTIO_PCI_CAP_PARSER_VIRTIO_NOTIFY_CAP_LEN) {
                    return VIRTIO_PCI_CAP_PARSE_ERR_NOTIFY_CAP_LEN_TOO_SMALL;
                }

                if ((size_t)current + (size_t)cap_len > cfg_space_len) {
                    return VIRTIO_PCI_CAP_PARSE_ERR_CAP_TRUNCATED;
                }

                bar = cfg_space[current + 4];
                id = cfg_space[current + 5];
                offset = virtio_pci_cap_parser_read_le32(cfg_space + current + 8);
                length = virtio_pci_cap_parser_read_le32(cfg_space + current + 12);

                if (cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON) {
                    if (found_common == 0 || length > out_caps->common_cfg.length) {
                        virtio_pci_cap_parse_result_t res = virtio_pci_cap_parser_store_region(
                            &out_caps->common_cfg, bar_addrs, bar, id, cap_len, current, offset, length);
                        if (res != VIRTIO_PCI_CAP_PARSE_OK) {
                            return res;
                        }
                        found_common = 1;
                    }
                } else if (cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_NOTIFY) {
                    if (found_notify == 0 || length > out_caps->notify_cfg.length) {
                        virtio_pci_cap_parse_result_t res = virtio_pci_cap_parser_store_region(
                            &out_caps->notify_cfg, bar_addrs, bar, id, cap_len, current, offset, length);
                        if (res != VIRTIO_PCI_CAP_PARSE_OK) {
                            return res;
                        }
                        out_caps->notify_off_multiplier = virtio_pci_cap_parser_read_le32(
                            cfg_space + current + VIRTIO_PCI_CAP_PARSER_VIRTIO_CAP_LEN);
                        found_notify = 1;
                    }
                } else if (cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR) {
                    if (found_isr == 0 || length > out_caps->isr_cfg.length) {
                        virtio_pci_cap_parse_result_t res = virtio_pci_cap_parser_store_region(
                            &out_caps->isr_cfg, bar_addrs, bar, id, cap_len, current, offset, length);
                        if (res != VIRTIO_PCI_CAP_PARSE_OK) {
                            return res;
                        }
                        found_isr = 1;
                    }
                } else if (cfg_type == VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE) {
                    if (found_device == 0 || length > out_caps->device_cfg.length) {
                        virtio_pci_cap_parse_result_t res = virtio_pci_cap_parser_store_region(
                            &out_caps->device_cfg, bar_addrs, bar, id, cap_len, current, offset, length);
                        if (res != VIRTIO_PCI_CAP_PARSE_OK) {
                            return res;
                        }
                        found_device = 1;
                    }
                }
            }
        }

        current = cap_next;
    }

    if (found_common == 0) {
        return VIRTIO_PCI_CAP_PARSE_ERR_MISSING_COMMON_CFG;
    }
    if (found_notify == 0) {
        return VIRTIO_PCI_CAP_PARSE_ERR_MISSING_NOTIFY_CFG;
    }
    if (found_isr == 0) {
        return VIRTIO_PCI_CAP_PARSE_ERR_MISSING_ISR_CFG;
    }
    if (found_device == 0) {
        return VIRTIO_PCI_CAP_PARSE_ERR_MISSING_DEVICE_CFG;
    }

    return VIRTIO_PCI_CAP_PARSE_OK;
}

const char *virtio_pci_cap_parse_result_str(virtio_pci_cap_parse_result_t result) {
    switch (result) {
        case VIRTIO_PCI_CAP_PARSE_OK:
            return "OK";
        case VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT:
            return "BAD_ARGUMENT";
        case VIRTIO_PCI_CAP_PARSE_ERR_CFG_SPACE_TOO_SMALL:
            return "CFG_SPACE_TOO_SMALL";
        case VIRTIO_PCI_CAP_PARSE_ERR_NO_CAP_LIST:
            return "NO_CAP_LIST";
        case VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_OUT_OF_RANGE:
            return "CAP_PTR_OUT_OF_RANGE";
        case VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_UNALIGNED:
            return "CAP_PTR_UNALIGNED";
        case VIRTIO_PCI_CAP_PARSE_ERR_CAP_HEADER_TRUNCATED:
            return "CAP_HEADER_TRUNCATED";
        case VIRTIO_PCI_CAP_PARSE_ERR_CAP_NEXT_OUT_OF_RANGE:
            return "CAP_NEXT_OUT_OF_RANGE";
        case VIRTIO_PCI_CAP_PARSE_ERR_CAP_LIST_LOOP:
            return "CAP_LIST_LOOP";
        case VIRTIO_PCI_CAP_PARSE_ERR_CAP_LEN_TOO_SMALL:
            return "CAP_LEN_TOO_SMALL";
        case VIRTIO_PCI_CAP_PARSE_ERR_NOTIFY_CAP_LEN_TOO_SMALL:
            return "NOTIFY_CAP_LEN_TOO_SMALL";
        case VIRTIO_PCI_CAP_PARSE_ERR_CAP_TRUNCATED:
            return "CAP_TRUNCATED";
        case VIRTIO_PCI_CAP_PARSE_ERR_BAR_INDEX_OUT_OF_RANGE:
            return "BAR_INDEX_OUT_OF_RANGE";
        case VIRTIO_PCI_CAP_PARSE_ERR_BAR_ADDRESS_MISSING:
            return "BAR_ADDRESS_MISSING";
        case VIRTIO_PCI_CAP_PARSE_ERR_DUPLICATE_CFG_TYPE:
            return "DUPLICATE_CFG_TYPE";
        case VIRTIO_PCI_CAP_PARSE_ERR_MISSING_COMMON_CFG:
            return "MISSING_COMMON_CFG";
        case VIRTIO_PCI_CAP_PARSE_ERR_MISSING_NOTIFY_CFG:
            return "MISSING_NOTIFY_CFG";
        case VIRTIO_PCI_CAP_PARSE_ERR_MISSING_ISR_CFG:
            return "MISSING_ISR_CFG";
        case VIRTIO_PCI_CAP_PARSE_ERR_MISSING_DEVICE_CFG:
            return "MISSING_DEVICE_CFG";
        default:
            return "UNKNOWN";
    }
}
