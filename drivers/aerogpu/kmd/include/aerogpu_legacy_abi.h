#pragma once

/*
 * Minimal legacy AeroGPU ABI definitions required by the Win7 WDDM KMD.
 *
 * The legacy ABI was formerly defined by `drivers/aerogpu/protocol/aerogpu_protocol.h`,
 * but that header macro-conflicts with the versioned ABI headers
 * (`aerogpu_pci.h` + `aerogpu_ring.h`). This header intentionally contains only
 * the subset of constants/structs required to keep the legacy device working.
 */

#include <stdint.h>

/* Legacy BAR0 identification */
#define AEROGPU_LEGACY_MMIO_MAGIC 0x41524750u /* 'A''R''G''P' */
#define AEROGPU_LEGACY_MMIO_VERSION 0x00010000u

/* Legacy MMIO register offsets (BAR0) */
#define AEROGPU_LEGACY_REG_MAGIC 0x0000u
#define AEROGPU_LEGACY_REG_VERSION 0x0004u

#define AEROGPU_LEGACY_REG_RING_BASE_LO 0x0010u
#define AEROGPU_LEGACY_REG_RING_BASE_HI 0x0014u
#define AEROGPU_LEGACY_REG_RING_ENTRY_COUNT 0x0018u
#define AEROGPU_LEGACY_REG_RING_HEAD 0x001Cu
#define AEROGPU_LEGACY_REG_RING_TAIL 0x0020u
#define AEROGPU_LEGACY_REG_RING_DOORBELL 0x0024u

#define AEROGPU_LEGACY_REG_INT_STATUS 0x0030u
#define AEROGPU_LEGACY_REG_INT_ACK 0x0034u
#define AEROGPU_LEGACY_REG_FENCE_COMPLETED 0x0038u

#define AEROGPU_LEGACY_REG_SCANOUT_FB_LO 0x0100u
#define AEROGPU_LEGACY_REG_SCANOUT_FB_HI 0x0104u
#define AEROGPU_LEGACY_REG_SCANOUT_PITCH 0x0108u
#define AEROGPU_LEGACY_REG_SCANOUT_WIDTH 0x010Cu
#define AEROGPU_LEGACY_REG_SCANOUT_HEIGHT 0x0110u
#define AEROGPU_LEGACY_REG_SCANOUT_FORMAT 0x0114u
#define AEROGPU_LEGACY_REG_SCANOUT_ENABLE 0x0118u

/* Legacy IRQ bits */
#define AEROGPU_LEGACY_INT_FENCE 0x00000001u

/* Legacy scanout format enum values */
#define AEROGPU_LEGACY_SCANOUT_X8R8G8B8 1u

/* Legacy ring entry types */
#define AEROGPU_LEGACY_RING_ENTRY_SUBMIT 1u

typedef struct aerogpu_legacy_ring_entry_submit {
    uint32_t type;      /* AEROGPU_LEGACY_RING_ENTRY_SUBMIT */
    uint32_t flags;     /* reserved */
    uint32_t fence;     /* monotonically increasing fence id */
    uint32_t desc_size; /* bytes */
    uint64_t desc_gpa;  /* guest physical address of submission descriptor */
} aerogpu_legacy_ring_entry_submit;

typedef union aerogpu_legacy_ring_entry {
    uint32_t type;
    aerogpu_legacy_ring_entry_submit submit;
} aerogpu_legacy_ring_entry;

/* Legacy submission descriptor structures */
#define AEROGPU_LEGACY_SUBMISSION_DESC_VERSION 1u

typedef struct aerogpu_legacy_submission_desc_header {
    uint32_t version; /* AEROGPU_LEGACY_SUBMISSION_DESC_VERSION */
    uint32_t type;    /* driver-private: AEROGPU_SUBMIT_* */
    uint32_t fence;   /* same as ring entry fence */
    uint32_t reserved0;

    uint64_t dma_buffer_gpa; /* guest physical address, contiguous */
    uint32_t dma_buffer_size;
    uint32_t allocation_count;
} aerogpu_legacy_submission_desc_header;

typedef struct aerogpu_legacy_submission_desc_allocation {
    uint64_t allocation_handle; /* driver-private token (for debugging) */
    uint64_t gpa;               /* base guest physical address */
    uint32_t size_bytes;
    uint32_t reserved0;
} aerogpu_legacy_submission_desc_allocation;

