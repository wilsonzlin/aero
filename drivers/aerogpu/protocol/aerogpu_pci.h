/*
 * AeroGPU Guest↔Emulator ABI (PCI/MMIO)
 *
 * This header is part of the stable, versioned contract between the Windows 7
 * AeroGPU WDDM driver (guest) and the Aero emulator (host).
 *
 * Requirements:
 * - Must compile as C/C++ under Windows driver toolchains (WDK10+ supported).
 * - All multi-byte fields are little-endian.
 * - MMIO registers are 32-bit wide unless documented otherwise.
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_PCI_H_
#define AEROGPU_PROTOCOL_AEROGPU_PCI_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <stdint.h>

/* -------------------------- Compile-time utilities ----------------------- */

#define AEROGPU_CONCAT2_(a, b) a##b
#define AEROGPU_CONCAT_(a, b) AEROGPU_CONCAT2_(a, b)
#define AEROGPU_STATIC_ASSERT(expr) \
  typedef char AEROGPU_CONCAT_(aerogpu_static_assert_, __LINE__)[(expr) ? 1 : -1]

/* ----------------------------- ABI versioning ---------------------------- */

/*
 * ABI versioning rules:
 * - Major changes are breaking (old drivers must not bind to new devices).
 * - Minor changes are backwards compatible (new fields/opcodes may be added).
 *
 * The ABI version is reported by MMIO register `AEROGPU_MMIO_REG_ABI_VERSION`.
 */
#define AEROGPU_ABI_MAJOR 1u
#define AEROGPU_ABI_MINOR 2u
#define AEROGPU_ABI_VERSION_U32 (((uint32_t)AEROGPU_ABI_MAJOR << 16) | (uint32_t)AEROGPU_ABI_MINOR)

/* ------------------------------- PCI identity ---------------------------- */

/*
 * NOTE: These PCI IDs are project-specific and are NOT assigned by PCI-SIG.
 * They are only intended for use inside the Aero emulator.
 */
#define AEROGPU_PCI_VENDOR_ID 0xA3A0u
#define AEROGPU_PCI_DEVICE_ID 0x0001u
#define AEROGPU_PCI_SUBSYSTEM_VENDOR_ID AEROGPU_PCI_VENDOR_ID
#define AEROGPU_PCI_SUBSYSTEM_ID 0x0001u

/*
 * PCI class code: Display controller.
 * - Base class 0x03: Display Controller
 * - Subclass  0x00: VGA compatible controller (widely accepted by Windows)
 */
#define AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER 0x03u
#define AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE 0x00u
#define AEROGPU_PCI_PROG_IF 0x00u

/* -------------------------------- BAR layout ----------------------------- */

/*
 * BAR0: MMIO register block.
 * The device model should expose at least 64 KiB to allow future expansion.
 */
#define AEROGPU_PCI_BAR0_INDEX 0u
#define AEROGPU_PCI_BAR0_SIZE_BYTES (64u * 1024u)

/* ------------------------------ MMIO registers --------------------------- */

/*
 * MMIO register access notes:
 * - All registers are little-endian.
 * - 64-bit values are split into LO/HI 32-bit halves at consecutive offsets.
 */

/* Identification / discovery */
#define AEROGPU_MMIO_REG_MAGIC 0x0000u /* RO: must read as AEROGPU_MMIO_MAGIC */
#define AEROGPU_MMIO_REG_ABI_VERSION 0x0004u /* RO: AEROGPU_ABI_VERSION_U32 */
#define AEROGPU_MMIO_REG_FEATURES_LO 0x0008u /* RO */
#define AEROGPU_MMIO_REG_FEATURES_HI 0x000Cu /* RO */

#define AEROGPU_MMIO_MAGIC 0x55504741u /* "AGPU" little-endian */

/* Device feature bits (FEATURES_LO/HI) */
#define AEROGPU_FEATURE_FENCE_PAGE (1ull << 0) /* Supports shared fence page */
#define AEROGPU_FEATURE_CURSOR (1ull << 1) /* Implements cursor registers */
#define AEROGPU_FEATURE_SCANOUT (1ull << 2) /* Implements scanout registers */
#define AEROGPU_FEATURE_VBLANK (1ull << 3) /* Implements vblank IRQ + vblank timing regs */
#define AEROGPU_FEATURE_TRANSFER (1ull << 4) /* Supports transfer/copy commands + optional guest writeback (ABI 1.1+) */

/* Ring setup */
#define AEROGPU_MMIO_REG_RING_GPA_LO 0x0100u /* RW: GPA of aerogpu_ring_header */
#define AEROGPU_MMIO_REG_RING_GPA_HI 0x0104u /* RW */
#define AEROGPU_MMIO_REG_RING_SIZE_BYTES 0x0108u /* RW: bytes mapped at RING_GPA (>= ring_header.size_bytes) */
#define AEROGPU_MMIO_REG_RING_CONTROL 0x010Cu /* RW */

/* Ring control bits */
#define AEROGPU_RING_CONTROL_ENABLE (1u << 0) /* Driver sets to 1 after init */
#define AEROGPU_RING_CONTROL_RESET (1u << 1) /* Write 1 to request ring reset */

/* Optional shared fence page (recommended for low MMIO polling overhead) */
#define AEROGPU_MMIO_REG_FENCE_GPA_LO 0x0120u /* RW: GPA of aerogpu_fence_page */
#define AEROGPU_MMIO_REG_FENCE_GPA_HI 0x0124u /* RW */

/* Completed fence value (always available, even without fence page) */
#define AEROGPU_MMIO_REG_COMPLETED_FENCE_LO 0x0130u /* RO */
#define AEROGPU_MMIO_REG_COMPLETED_FENCE_HI 0x0134u /* RO */

/* Doorbell (write-only): notify device that new submissions are available */
#define AEROGPU_MMIO_REG_DOORBELL 0x0200u /* WO */

/* Interrupts */
#define AEROGPU_MMIO_REG_IRQ_STATUS 0x0300u /* RO */
#define AEROGPU_MMIO_REG_IRQ_ENABLE 0x0304u /* RW */
#define AEROGPU_MMIO_REG_IRQ_ACK 0x0308u /* WO: write-1-to-clear */

/* IRQ_STATUS / IRQ_ENABLE bits */
#define AEROGPU_IRQ_FENCE (1u << 0) /* Completed fence advanced */
#define AEROGPU_IRQ_SCANOUT_VBLANK (1u << 1) /* Scanout vblank tick (if AEROGPU_FEATURE_VBLANK) */
#define AEROGPU_IRQ_ERROR (1u << 31) /* Fatal device error */

/* Scanout 0 configuration */
#define AEROGPU_MMIO_REG_SCANOUT0_ENABLE 0x0400u /* RW */
#define AEROGPU_MMIO_REG_SCANOUT0_WIDTH 0x0404u /* RW */
#define AEROGPU_MMIO_REG_SCANOUT0_HEIGHT 0x0408u /* RW */
#define AEROGPU_MMIO_REG_SCANOUT0_FORMAT 0x040Cu /* RW: aerogpu_format */
#define AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES 0x0410u /* RW */
#define AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO 0x0414u /* RW */
#define AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI 0x0418u /* RW */

/*
 * Scanout 0 vblank timing (if AEROGPU_FEATURE_VBLANK is set).
 *
 * These registers are intended to support Windows 7 WDDM vblank wait paths
 * (D3DKMTWaitForVerticalBlankEvent) and scanline/raster status queries.
 */
#define AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO 0x0420u /* RO */
#define AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI 0x0424u /* RO */
#define AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO 0x0428u /* RO */
#define AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI 0x042Cu /* RO */
#define AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS 0x0430u /* RO: nominal period in ns */

/* Cursor configuration (reserved if AEROGPU_FEATURE_CURSOR == 0) */
#define AEROGPU_MMIO_REG_CURSOR_ENABLE 0x0500u /* RW */
#define AEROGPU_MMIO_REG_CURSOR_X 0x0504u /* RW: signed 32-bit */
#define AEROGPU_MMIO_REG_CURSOR_Y 0x0508u /* RW: signed 32-bit */
#define AEROGPU_MMIO_REG_CURSOR_HOT_X 0x050Cu /* RW */
#define AEROGPU_MMIO_REG_CURSOR_HOT_Y 0x0510u /* RW */
#define AEROGPU_MMIO_REG_CURSOR_WIDTH 0x0514u /* RW */
#define AEROGPU_MMIO_REG_CURSOR_HEIGHT 0x0518u /* RW */
#define AEROGPU_MMIO_REG_CURSOR_FORMAT 0x051Cu /* RW: aerogpu_format */
#define AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO 0x0520u /* RW */
#define AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI 0x0524u /* RW */
#define AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES 0x0528u /* RW */

/* ------------------------------- Shared enums ---------------------------- */

/*
 * Resource / scanout formats.
 * Values are stable once published. Unknown values must be treated as invalid.
 */
enum aerogpu_format {
  AEROGPU_FORMAT_INVALID = 0,

  /* Common BGRA/RGBA formats */
  AEROGPU_FORMAT_B8G8R8A8_UNORM = 1,
  AEROGPU_FORMAT_B8G8R8X8_UNORM = 2,
  AEROGPU_FORMAT_R8G8B8A8_UNORM = 3,
  AEROGPU_FORMAT_R8G8B8X8_UNORM = 4,

  /* 16-bit RGB */
  AEROGPU_FORMAT_B5G6R5_UNORM = 5,
  AEROGPU_FORMAT_B5G5R5A1_UNORM = 6,

  /* Common BGRA/RGBA sRGB formats */
  AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB = 7,
  AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB = 8,
  AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB = 9,
  AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB = 10,

  /* Depth/stencil (for future D3D10/11) */
  AEROGPU_FORMAT_D24_UNORM_S8_UINT = 32,
  AEROGPU_FORMAT_D32_FLOAT = 33,

  /* Block-compressed formats (4x4 blocks) */
  AEROGPU_FORMAT_BC1_RGBA_UNORM = 64,
  AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB = 65,
  AEROGPU_FORMAT_BC2_RGBA_UNORM = 66,
  AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB = 67,
  AEROGPU_FORMAT_BC3_RGBA_UNORM = 68,
  AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB = 69,
  AEROGPU_FORMAT_BC7_RGBA_UNORM = 70,
  AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB = 71,
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_PCI_H_ */
