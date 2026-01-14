#pragma once

/*
 * Shared miniport IOCTL contract for aero_virtio_blk.
 *
 * This header is intentionally WDK-free so it can be included by both:
 *   - kernel-mode miniport driver (aero_virtio_blk.sys)
 *   - user-mode guest selftest (aero-virtio-selftest.exe)
 *
 * Keeping the IOCTL structs/constants in one place prevents silent layout drift.
 */

#include <stdint.h>

/*
 * SRB_IO_CONTROL.Signature for aero_virtio_blk miniport IOCTLs.
 *
 * Note: SRB_IO_CONTROL.Signature is 8 bytes; callers should copy exactly 8
 * bytes (no NUL terminator required).
 */
#define AEROVBLK_SRBIO_SIG "AEROVBLK"

/* SRB_IO_CONTROL.ControlCode values. */
#define AEROVBLK_IOCTL_QUERY 0x8000A001u
#define AEROVBLK_IOCTL_FORCE_RESET 0x8000A002u

/*
 * AEROVBLK_QUERY_INFO.InterruptMode values.
 *
 * The effective interrupt mode can be INTx (shared line + ISR status byte) or
 * message-signaled interrupts (MSI/MSI-X).
 */
#define AEROVBLK_INTERRUPT_MODE_INTX 0u
#define AEROVBLK_INTERRUPT_MODE_MSI 1u

#pragma pack(push, 1)
typedef struct _AEROVBLK_QUERY_INFO {
    uint64_t NegotiatedFeatures;
    uint16_t QueueSize;
    uint16_t NumFree;
    uint16_t AvailIdx;
    uint16_t UsedIdx;

    /*
     * Interrupt observability (virtio-pci modern).
     *
     * These fields are appended for backwards compatibility: callers that only
     * understand the original v1 layout can request/consume just the first 16
     * bytes (through UsedIdx).
     */
    uint32_t InterruptMode;
    uint16_t MsixConfigVector;
    uint16_t MsixQueue0Vector;
    uint32_t MessageCount;
    uint32_t Reserved0;

    /* SRB function counters (appended). */
    uint32_t AbortSrbCount;
    uint32_t ResetDeviceSrbCount;
    uint32_t ResetBusSrbCount;
    uint32_t PnpSrbCount;
    uint32_t IoctlResetCount;

    /* Optional (appended): number of capacity change events handled at runtime. */
    uint32_t CapacityChangeEvents;
} AEROVBLK_QUERY_INFO, *PAEROVBLK_QUERY_INFO;
#pragma pack(pop)
