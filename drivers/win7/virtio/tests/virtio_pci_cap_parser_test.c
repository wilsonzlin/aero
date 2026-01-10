#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "virtio_pci_cap_parser.h"

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

static void test_duplicated_cap_type(void) {
    uint8_t cfg[256];
    uint64_t bars[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t res;

    memset(cfg, 0, sizeof(cfg));
    memset(bars, 0, sizeof(bars));

    write_le16(&cfg[VIRTIO_PCI_CAP_PARSER_PCI_STATUS_OFFSET], VIRTIO_PCI_CAP_PARSER_PCI_STATUS_CAP_LIST);
    cfg[VIRTIO_PCI_CAP_PARSER_PCI_CAP_PTR_OFFSET] = 0x40;

    add_virtio_cap(cfg, 0x40, 0x50, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1000, 0x100, 16);
    add_virtio_cap(cfg, 0x50, 0x64, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x1100, 0x100, 16);
    add_virtio_notify_cap(cfg, 0x64, 0x78, 2, 0x2000, 0x200, 4);
    add_virtio_cap(cfg, 0x78, 0x88, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 1, 0x3000, 0x10, 16);
    add_virtio_cap(cfg, 0x88, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 4, 0x4000, 0x400, 16);

    bars[0] = 0xA0000000ULL;
    bars[1] = 0xB0000000ULL;
    bars[2] = 0xC0000000ULL;
    bars[4] = 0xD0000000ULL;

    res = virtio_pci_cap_parse(cfg, sizeof(cfg), bars, &caps);
    expect_result("duplicated_cap_type.res", res, VIRTIO_PCI_CAP_PARSE_ERR_DUPLICATE_CFG_TYPE);
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

int main(void) {
    test_valid_all_caps();
    test_duplicated_cap_type();
    test_missing_notify_cap();
    test_looping_cap_list();
    test_cap_len_too_short();

    if (tests_failed == 0) {
        printf("virtio_pci_cap_parser_test: %d checks passed\n", tests_run);
        return 0;
    }

    printf("virtio_pci_cap_parser_test: %d/%d checks failed\n", tests_failed, tests_run);
    return 1;
}
