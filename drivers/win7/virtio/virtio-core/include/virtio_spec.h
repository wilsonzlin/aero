#ifndef VIRTIO_SPEC_H_
#define VIRTIO_SPEC_H_

/*
 * Minimal Virtio 1.0+ structures/constants needed by the Win7 virtio-core
 * transport layer.
 *
 * This header intentionally avoids any driver/framework-specific dependencies
 * so it can be shared across virtio-* drivers.
 */

/*
 * This header is shared between kernel-mode drivers and host-buildable unit
 * tests. Avoid unconditional WDK dependencies so the definitions (notably
 * `virtio_pci_common_cfg`) can be compiled on CI without requiring the Windows
 * driver kit.
 */

#include <stddef.h>

#if defined(_WIN32) && (defined(_KERNEL_MODE) || defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_))
/* Kernel-mode build (WDK headers available). */
#include <ntddk.h>
#else
/* User-mode / non-WDK build: provide WDK-compatible typedefs. */
#include <stdint.h>

typedef uint8_t UINT8;
typedef uint16_t UINT16;
typedef uint32_t UINT32;
typedef uint64_t UINT64;

#ifndef FIELD_OFFSET
#define FIELD_OFFSET(type, field) (offsetof(type, field))
#endif

#ifndef C_ASSERT
#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L)
#define C_ASSERT(expr) _Static_assert(expr, #expr)
#else
#define VIRTIO_SPEC_CONCAT_INNER(a, b) a##b
#define VIRTIO_SPEC_CONCAT(a, b) VIRTIO_SPEC_CONCAT_INNER(a, b)
#define C_ASSERT(expr) typedef char VIRTIO_SPEC_CONCAT(C_ASSERT_, __LINE__)[(expr) ? 1 : -1]
#endif
#endif
#endif

#ifndef VIRTIO_PCI_MAX_BARS
#define VIRTIO_PCI_MAX_BARS 6
#endif

/* Virtio 1.0 feature bit indicating a modern (1.0+) device. */
#ifndef VIRTIO_F_VERSION_1
#define VIRTIO_F_VERSION_1 ((UINT64)1u << 32)
#endif

/* Common device status bits (virtio spec "Device Status Field"). */
#define VIRTIO_STATUS_ACKNOWLEDGE        0x01
#define VIRTIO_STATUS_DRIVER             0x02
#define VIRTIO_STATUS_DRIVER_OK          0x04
#define VIRTIO_STATUS_FEATURES_OK        0x08
#define VIRTIO_STATUS_DEVICE_NEEDS_RESET 0x40
#define VIRTIO_STATUS_FAILED             0x80

/*
 * Compatibility aliases.
 *
 * Some Virtio codebases (including the task spec for this repo) use the
 * VIRTIO_CONFIG_S_* naming. Keep the canonical VIRTIO_STATUS_* names and
 * provide aliases to avoid churn.
 */
#ifndef VIRTIO_CONFIG_S_ACKNOWLEDGE
#define VIRTIO_CONFIG_S_ACKNOWLEDGE VIRTIO_STATUS_ACKNOWLEDGE
#endif
#ifndef VIRTIO_CONFIG_S_DRIVER
#define VIRTIO_CONFIG_S_DRIVER VIRTIO_STATUS_DRIVER
#endif
#ifndef VIRTIO_CONFIG_S_DRIVER_OK
#define VIRTIO_CONFIG_S_DRIVER_OK VIRTIO_STATUS_DRIVER_OK
#endif
#ifndef VIRTIO_CONFIG_S_FEATURES_OK
#define VIRTIO_CONFIG_S_FEATURES_OK VIRTIO_STATUS_FEATURES_OK
#endif
#ifndef VIRTIO_CONFIG_S_DEVICE_NEEDS_RESET
#define VIRTIO_CONFIG_S_DEVICE_NEEDS_RESET VIRTIO_STATUS_DEVICE_NEEDS_RESET
#endif
#ifndef VIRTIO_CONFIG_S_FAILED
#define VIRTIO_CONFIG_S_FAILED VIRTIO_STATUS_FAILED
#endif

#pragma pack(push, 1)

/*
 * Virtio PCI "common configuration" structure (virtio spec:
 * "Virtio Over PCI Bus -> Common configuration structure").
 *
 * Note: The spec defines 64-bit queue addresses. This struct exposes both the
 * 64-bit fields and 32-bit lo/hi views so code can safely use 32-bit MMIO
 * accessors on WDK7.
 */
typedef struct virtio_pci_common_cfg {
    UINT32 device_feature_select; /* read-write */
    UINT32 device_feature;        /* read-only  */
    UINT32 driver_feature_select; /* read-write */
    UINT32 driver_feature;        /* read-write */
    UINT16 msix_config;           /* read-write */
    UINT16 num_queues;            /* read-only  */
    UINT8 device_status;          /* read-write */
    UINT8 config_generation;      /* read-only  */

    UINT16 queue_select;      /* read-write */
    UINT16 queue_size;        /* read-only  */
    UINT16 queue_msix_vector; /* read-write */
    UINT16 queue_enable;      /* read-write */
    UINT16 queue_notify_off;  /* read-only  */

    union {
        UINT64 queue_desc; /* read-write (virtio spec: __le64 queue_desc) */
        struct {
            UINT32 queue_desc_lo; /* read-write */
            UINT32 queue_desc_hi; /* read-write */
        };
    };

    union {
        UINT64 queue_avail; /* read-write (virtio spec: __le64 queue_avail) */
        struct {
            UINT32 queue_avail_lo; /* read-write */
            UINT32 queue_avail_hi; /* read-write */
        };
    };

    union {
        UINT64 queue_used; /* read-write (virtio spec: __le64 queue_used) */
        struct {
            UINT32 queue_used_lo; /* read-write */
            UINT32 queue_used_hi; /* read-write */
        };
    };
} virtio_pci_common_cfg, *Pvirtio_pci_common_cfg;

#pragma pack(pop)

/*
 * CommonCfg offsets are defined by the virtio spec. Assert the layout so any
 * accidental padding or stray fields are caught at compile time.
 */
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_feature_select) == 0x00);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_feature) == 0x04);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, driver_feature_select) == 0x08);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, driver_feature) == 0x0C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, msix_config) == 0x10);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, num_queues) == 0x12);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_status) == 0x14);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, config_generation) == 0x15);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_select) == 0x16);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_size) == 0x18);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_msix_vector) == 0x1A);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_enable) == 0x1C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_notify_off) == 0x1E);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc) == 0x20);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc_lo) == 0x20);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc_hi) == 0x24);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail) == 0x28);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail_lo) == 0x28);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail_hi) == 0x2C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used) == 0x30);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used_lo) == 0x30);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used_hi) == 0x34);
C_ASSERT(sizeof(virtio_pci_common_cfg) == 0x38);

#endif /* VIRTIO_SPEC_H_ */
