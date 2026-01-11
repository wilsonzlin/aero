#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "virtio_pci_cap_parser.h"
#include "virtio_pci_aero_layout.h"
#include "virtio_pci_identity.h"

static void write_le16(uint8_t *dst, uint16_t v) {
    dst[0] = (uint8_t)(v & 0xffu);
    dst[1] = (uint8_t)((v >> 8) & 0xffu);
}

static void write_le32(uint8_t *dst, uint32_t v) {
    dst[0] = (uint8_t)(v & 0xffu);
    dst[1] = (uint8_t)((v >> 8) & 0xffu);
    dst[2] = (uint8_t)((v >> 16) & 0xffu);
    dst[3] = (uint8_t)((v >> 24) & 0xffu);
}

static void add_virtio_cap(
    uint8_t cfg[256],
    uint8_t cap_off,
    uint8_t cap_next,
    uint8_t cfg_type,
    uint8_t bar,
    uint32_t region_off,
    uint32_t region_len,
    uint8_t cap_len) {
    cfg[cap_off + 0] = VIRTIO_PCI_CAP_PARSER_PCI_CAP_ID_VNDR;
    cfg[cap_off + 1] = cap_next;
    cfg[cap_off + 2] = cap_len;
    cfg[cap_off + 3] = cfg_type;
    cfg[cap_off + 4] = bar;
    cfg[cap_off + 5] = 0;
    cfg[cap_off + 6] = 0;
    cfg[cap_off + 7] = 0;
    write_le32(&cfg[cap_off + 8], region_off);
    write_le32(&cfg[cap_off + 12], region_len);
}

static void add_virtio_notify_cap(
    uint8_t cfg[256],
    uint8_t cap_off,
    uint8_t cap_next,
    uint8_t bar,
    uint32_t region_off,
    uint32_t region_len,
    uint32_t mult) {
    add_virtio_cap(
        cfg,
        cap_off,
        cap_next,
        VIRTIO_PCI_CAP_PARSER_CFG_TYPE_NOTIFY,
        bar,
        region_off,
        region_len,
        20);
    write_le32(&cfg[cap_off + 16], mult);
}

static int tests_failed = 0;
static int tests_run = 0;

static void expect_result(
    const char *name,
    virtio_pci_cap_parse_result_t got,
    virtio_pci_cap_parse_result_t want) {
    ++tests_run;
    if (got == want) {
        return;
    }
    ++tests_failed;
    fprintf(stderr, "FAIL %s: got=%s (%d) want=%s (%d)\n",
        name,
        virtio_pci_cap_parse_result_str(got),
        (int)got,
        virtio_pci_cap_parse_result_str(want),
        (int)want);
}

static void expect_identity_result(
    const char *name,
    virtio_pci_identity_result_t got,
    virtio_pci_identity_result_t want) {
    ++tests_run;
    if (got == want) {
        return;
    }
    ++tests_failed;
    fprintf(stderr, "FAIL %s: got=%s (%d) want=%s (%d)\n",
        name,
        virtio_pci_identity_result_str(got),
        (int)got,
        virtio_pci_identity_result_str(want),
        (int)want);
}

static void expect_u64(const char *name, uint64_t got, uint64_t want) {
    ++tests_run;
    if (got == want) {
        return;
    }
    ++tests_failed;
    fprintf(stderr, "FAIL %s: got=0x%llx want=0x%llx\n",
        name,
        (unsigned long long)got,
        (unsigned long long)want);
}

static void expect_u32(const char *name, uint32_t got, uint32_t want) {
    ++tests_run;
    if (got == want) {
        return;
    }
    ++tests_failed;
    fprintf(stderr, "FAIL %s: got=0x%x want=0x%x\n", name, got, want);
}

static void expect_layout_result(
    const char *name,
    virtio_pci_aero_layout_validate_result_t got,
    virtio_pci_aero_layout_validate_result_t want) {
    ++tests_run;
    if (got == want) {
        return;
    }
    ++tests_failed;
    fprintf(stderr, "FAIL %s: got=%s (%d) want=%s (%d)\n",
            name,
            virtio_pci_aero_layout_validate_result_str(got),
            (int)got,
            virtio_pci_aero_layout_validate_result_str(want),
            (int)want);
}

static void test_valid_all_caps(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x80, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("valid_all_caps.res", res, VIRTIO_PCI_CAP_PARSE_OK);
    if (res != VIRTIO_PCI_CAP_PARSE_OK) {
        return;
    }

    expect_u64("valid_all_caps.common.addr", caps.common_cfg.addr, 0xA0001000ULL);
    expect_u64("valid_all_caps.notify.addr", caps.notify_cfg.addr, 0xC0002000ULL);
    expect_u64("valid_all_caps.isr.addr", caps.isr_cfg.addr, 0xB0003000ULL);
    expect_u64("valid_all_caps.device.addr", caps.device_cfg.addr, 0xD0004000ULL);
    expect_u32("valid_all_caps.notify.mult", caps.notify_off_multiplier, 4);
}

static void test_aero_layout_validation_ok(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t parseRes;
    virtio_pci_bar_info_t barInfo[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_aero_layout_validate_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));
    memset(&caps, 0, sizeof(caps));
    memset(barInfo, 0, sizeof(barInfo));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x0000, 0x0100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 0, 0x1000, 0x0100, 4);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 0, 0x2000, 0x0020, 16);
    add_virtio_cap(cfg, 0x80, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 0, 0x3000, 0x0100, 16);

    bars[0] = 0xA0000000ULL;

    parseRes = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("aero_layout_validation_ok.parse", parseRes, VIRTIO_PCI_CAP_PARSE_OK);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        return;
    }

    barInfo[0].present = 1;
    barInfo[0].is_memory = 1;
    barInfo[0].length = 0x4000;

    res = virtio_pci_validate_aero_pci_layout(&caps, barInfo, VIRTIO_PCI_LAYOUT_POLICY_PERMISSIVE);
    expect_layout_result("aero_layout_validation_ok.permissive", res, VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK);

    res = virtio_pci_validate_aero_pci_layout(&caps, barInfo, VIRTIO_PCI_LAYOUT_POLICY_AERO_STRICT);
    expect_layout_result("aero_layout_validation_ok.strict", res, VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK);
}

static void test_aero_layout_validation_offset_mismatch(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t parseRes;
    virtio_pci_bar_info_t barInfo[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_aero_layout_validate_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));
    memset(&caps, 0, sizeof(caps));
    memset(barInfo, 0, sizeof(barInfo));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    /* Valid modern virtio, but with a different BAR0 capability placement. */
    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x0100, 0x0100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 0, 0x1100, 0x0100, 4);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 0, 0x2100, 0x0020, 16);
    add_virtio_cap(cfg, 0x80, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 0, 0x3100, 0x0100, 16);

    bars[0] = 0xA0000000ULL;

    parseRes = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("aero_layout_validation_offset_mismatch.parse", parseRes, VIRTIO_PCI_CAP_PARSE_OK);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        return;
    }

    barInfo[0].present = 1;
    barInfo[0].is_memory = 1;
    barInfo[0].length = 0x4000;

    res = virtio_pci_validate_aero_pci_layout(&caps, barInfo, VIRTIO_PCI_LAYOUT_POLICY_PERMISSIVE);
    expect_layout_result("aero_layout_validation_offset_mismatch.permissive", res, VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK);

    res = virtio_pci_validate_aero_pci_layout(&caps, barInfo, VIRTIO_PCI_LAYOUT_POLICY_AERO_STRICT);
    expect_layout_result("aero_layout_validation_offset_mismatch.strict", res, VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_COMMON_MISMATCH);
}

static void test_aero_layout_validation_notify_multiplier_mismatch(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t parseRes;
    virtio_pci_bar_info_t barInfo[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_aero_layout_validate_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));
    memset(&caps, 0, sizeof(caps));
    memset(barInfo, 0, sizeof(barInfo));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x0000, 0x0100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 0, 0x1000, 0x0100, 8);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 0, 0x2000, 0x0020, 16);
    add_virtio_cap(cfg, 0x80, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 0, 0x3000, 0x0100, 16);

    bars[0] = 0xA0000000ULL;

    parseRes = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("aero_layout_validation_notify_multiplier_mismatch.parse", parseRes, VIRTIO_PCI_CAP_PARSE_OK);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        return;
    }

    barInfo[0].present = 1;
    barInfo[0].is_memory = 1;
    barInfo[0].length = 0x4000;

    res = virtio_pci_validate_aero_pci_layout(&caps, barInfo, VIRTIO_PCI_LAYOUT_POLICY_PERMISSIVE);
    expect_layout_result("aero_layout_validation_notify_multiplier_mismatch.permissive",
                         res,
                         VIRTIO_PCI_AERO_LAYOUT_VALIDATE_OK);

    res = virtio_pci_validate_aero_pci_layout(&caps, barInfo, VIRTIO_PCI_LAYOUT_POLICY_AERO_STRICT);
    expect_layout_result("aero_layout_validation_notify_multiplier_mismatch.strict",
                         res,
                         VIRTIO_PCI_AERO_LAYOUT_VALIDATE_ERR_NOTIFY_MULTIPLIER_MISMATCH);
}

static void test_duplicated_cap_type(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x6C, 2, 0x2000, 0x100, 4);
    add_virtio_notify_cap(cfg, 0x6C, 0x80, 2, 0x2100, 0x200, 8);
    add_virtio_cap(cfg, 0x80, 0x90, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x90, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("duplicated_cap_type.res", res, VIRTIO_PCI_CAP_PARSE_OK);
    if (res != VIRTIO_PCI_CAP_PARSE_OK) {
        return;
    }

    expect_u64("duplicated_cap_type.notify.addr", caps.notify_cfg.addr, 0xC0002100ULL);
    expect_u32("duplicated_cap_type.notify.mult", caps.notify_off_multiplier, 8);
}

static void test_missing_notify_cap(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_cap(cfg, 0x54, 0x68, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x68, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("missing_notify_cap.res", res, VIRTIO_PCI_CAP_PARSE_ERR_MISSING_NOTIFY_CFG);
}

static void test_looping_cap_list(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x80, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("looping_cap_list.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_LIST_LOOP);
}

static void test_cap_len_too_short(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 8);
    bars[0] = 0xA0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("cap_len_too_short.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_LEN_TOO_SMALL);
}

static void test_notify_cap_len_too_short(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x50, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_cap(cfg, 0x50, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_NOTIFY, 2, 0x2000, 0x200, 16);

    bars[0] = 0xA0000000ULL;
    bars[2] = 0xC0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("notify_cap_len_too_short.res", res, VIRTIO_PCI_CAP_PARSE_ERR_NOTIFY_CAP_LEN_TOO_SMALL);
}

static void test_bar_address_missing(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x80, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    /* bars[0] intentionally left as 0 to simulate missing/mis-decoded BAR. */
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("bar_address_missing.res", res, VIRTIO_PCI_CAP_PARSE_ERR_BAR_ADDRESS_MISSING);
}

static void test_cap_ptr_unaligned(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x41;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x80, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("cap_ptr_unaligned.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_UNALIGNED);
}

static void test_cap_next_out_of_range(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    /* cap_next points beyond cfg_space_len (we pass a shorter length below). */
    add_virtio_cap(cfg, 0x40, 0xF0, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    bars[0] = 0xA0000000ULL;

    res = virtio_pci_cap_parse(cfg, 0x80, bars, &caps);
    expect_result("cap_next_out_of_range.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_NEXT_OUT_OF_RANGE);
}

static void test_cap_truncated(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x70;

    /* cap_len extends beyond cfg_space_len (we pass a shorter length below). */
    add_virtio_cap(cfg, 0x70, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 80);
    bars[0] = 0xA0000000ULL;

    res = virtio_pci_cap_parse(cfg, 0x80, bars, &caps);
    expect_result("cap_truncated.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_TRUNCATED);
}

static void test_bar_index_out_of_range(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 6, 0x1000, 0x100, 16);

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("bar_index_out_of_range.res", res, VIRTIO_PCI_CAP_PARSE_ERR_BAR_INDEX_OUT_OF_RANGE);
}

static void test_unknown_vendor_cap_ignored(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, 0x99, 0, 0x5000, 0x100, 16);
    add_virtio_cap(cfg, 0x54, 0x70, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x70, 0x84, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x84, 0x94, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x94, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("unknown_vendor_cap_ignored.res", res, VIRTIO_PCI_CAP_PARSE_OK);
}

static void test_cfg_space_too_small(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    res = virtio_pci_cap_parse(cfg, 0x20, bars, &caps);
    expect_result("cfg_space_too_small.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CFG_SPACE_TOO_SMALL);
}

static void test_no_cap_list_status(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    /* Status bit is clear, but cap_ptr and capabilities are still valid. */
    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], 0);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x80, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x80, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("no_cap_list_status.res", res, VIRTIO_PCI_CAP_PARSE_ERR_NO_CAP_LIST);
}

static void test_cap_ptr_out_of_range(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    /* cap_ptr is exactly cfg_space_len => out of range. */
    res = virtio_pci_cap_parse(cfg, 0x40, bars, &caps);
    expect_result("cap_ptr_out_of_range.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_PTR_OUT_OF_RANGE);
}

static void test_cap_header_truncated(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    /* cfg_space_len is too small to even read cap_id/cap_next at 0x40. */
    res = virtio_pci_cap_parse(cfg, 0x41, bars, &caps);
    expect_result("cap_header_truncated.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_HEADER_TRUNCATED);
}

static void test_cap_next_unaligned(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x55, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x73, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x82, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x80, 0x03, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("cap_next_unaligned.res", res, VIRTIO_PCI_CAP_PARSE_ERR_CAP_NEXT_UNALIGNED);
}

static void test_bad_argument_null_cfg_space(void) {
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(bars, 0, sizeof(bars));

    res = virtio_pci_cap_parse(NULL, 256, bars, &caps);
    expect_result("bad_argument_null_cfg_space.res", res, VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT);
}

static void test_bad_argument_null_bars(void) {
    uint8_t cfg[256];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), NULL, &caps);
    expect_result("bad_argument_null_bars.res", res, VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT);
}

static void test_bad_argument_null_out_caps(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, NULL);
    expect_result("bad_argument_null_out_caps.res", res, VIRTIO_PCI_CAP_PARSE_ERR_BAD_ARGUMENT);
}

static void test_no_cap_list_ptr_zero(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x00;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("no_cap_list_ptr_zero.res", res, VIRTIO_PCI_CAP_PARSE_ERR_NO_CAP_LIST);
}

static void test_non_vendor_cap_ignored(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    /* Capability ID 0x05 (MSI) with next pointer to the virtio vendor caps. */
    cfg[0x40] = 0x05;
    cfg[0x41] = 0x50;

    add_virtio_cap(cfg, 0x50, 0x64, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x64, 0x78, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x78, 0x88, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x88, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("non_vendor_cap_ignored.res", res, VIRTIO_PCI_CAP_PARSE_OK);
}

static void test_missing_common_cfg(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_notify_cap(cfg, 0x40, 0x60, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x60, 0x74, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x74, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("missing_common_cfg.res", res, VIRTIO_PCI_CAP_PARSE_ERR_MISSING_COMMON_CFG);
}

static void test_missing_isr_cfg(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("missing_isr_cfg.res", res, VIRTIO_PCI_CAP_PARSE_ERR_MISSING_ISR_CFG);
}

static void test_missing_device_cfg(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x54, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x54, 0x70, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x70, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("missing_device_cfg.res", res, VIRTIO_PCI_CAP_PARSE_ERR_MISSING_DEVICE_CFG);
}

static void test_identity_contract_v1_ok(void) {
    uint8_t cfg[256];
    const uint16_t allowed_ids[] = { 0x1052 };
    virtio_pci_identity_t id;
    virtio_pci_identity_result_t res;

    memset(cfg, 0, sizeof(cfg));
    write_le16(&cfg[0x00], VIRTIO_PCI_IDENTITY_VENDOR_ID_VIRTIO);
    write_le16(&cfg[0x02], 0x1052);
    cfg[0x08] = VIRTIO_PCI_IDENTITY_AERO_CONTRACT_V1_REVISION_ID;

    res = virtio_pci_identity_validate_aero_contract_v1(
        cfg, sizeof(cfg),
        allowed_ids, sizeof(allowed_ids) / sizeof(allowed_ids[0]),
        &id);

    expect_identity_result("identity_contract_v1_ok.res", res, VIRTIO_PCI_IDENTITY_OK);
    expect_u32("identity_contract_v1_ok.vendor", id.vendor_id, VIRTIO_PCI_IDENTITY_VENDOR_ID_VIRTIO);
    expect_u32("identity_contract_v1_ok.device", id.device_id, 0x1052);
    expect_u32("identity_contract_v1_ok.revision", id.revision_id, VIRTIO_PCI_IDENTITY_AERO_CONTRACT_V1_REVISION_ID);
}

static void test_identity_contract_v1_bad_revision(void) {
    uint8_t cfg[256];
    const uint16_t allowed_ids[] = { 0x1052 };
    virtio_pci_identity_t id;
    virtio_pci_identity_result_t res;

    memset(cfg, 0, sizeof(cfg));
    write_le16(&cfg[0x00], VIRTIO_PCI_IDENTITY_VENDOR_ID_VIRTIO);
    write_le16(&cfg[0x02], 0x1052);
    cfg[0x08] = 0x02; /* Unknown major version. */

    res = virtio_pci_identity_validate_aero_contract_v1(
        cfg, sizeof(cfg),
        allowed_ids, sizeof(allowed_ids) / sizeof(allowed_ids[0]),
        &id);

    expect_identity_result("identity_contract_v1_bad_revision.res", res, VIRTIO_PCI_IDENTITY_ERR_REVISION_MISMATCH);
}

int main(void) {
    test_valid_all_caps();
    test_aero_layout_validation_ok();
    test_aero_layout_validation_offset_mismatch();
    test_aero_layout_validation_notify_multiplier_mismatch();
    test_duplicated_cap_type();
    test_missing_notify_cap();
    test_looping_cap_list();
    test_cap_len_too_short();
    test_notify_cap_len_too_short();
    test_bar_address_missing();
    test_cap_ptr_unaligned();
    test_cap_next_out_of_range();
    test_cap_truncated();
    test_bar_index_out_of_range();
    test_unknown_vendor_cap_ignored();
    test_cfg_space_too_small();
    test_no_cap_list_status();
    test_cap_ptr_out_of_range();
    test_cap_header_truncated();
    test_cap_next_unaligned();
    test_bad_argument_null_cfg_space();
    test_bad_argument_null_bars();
    test_bad_argument_null_out_caps();
    test_no_cap_list_ptr_zero();
    test_non_vendor_cap_ignored();
    test_missing_common_cfg();
    test_missing_isr_cfg();
    test_missing_device_cfg();
    test_identity_contract_v1_ok();
    test_identity_contract_v1_bad_revision();

    if (tests_failed == 0) {
        printf("virtio_pci_cap_parser_test: %d checks passed\n", tests_run);
        return 0;
    }

    printf("virtio_pci_cap_parser_test: %d/%d checks failed\n", tests_failed, tests_run);
    return 1;
}
