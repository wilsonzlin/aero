// AeroGPU PCI/MMIO constants and ABI version helpers.
//
// Source of truth: `drivers/aerogpu/protocol/aerogpu_pci.h`.

export const AEROGPU_ABI_MAJOR = 1;
export const AEROGPU_ABI_MINOR = 4;
export const AEROGPU_ABI_VERSION_U32 = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;

export function abiMajor(versionU32: number): number {
  return (versionU32 >>> 16) & 0xffff;
}

export function abiMinor(versionU32: number): number {
  return versionU32 & 0xffff;
}

export class AerogpuAbiError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AerogpuAbiError";
  }
}

/**
 * Parse + validate an ABI version.
 *
 * Versioning rules:
 * - reject unsupported major versions
 * - accept unknown minor versions (treat as backwards-compatible extensions)
 */
export function parseAndValidateAbiVersionU32(versionU32: number): { major: number; minor: number } {
  const major = abiMajor(versionU32);
  const minor = abiMinor(versionU32);

  if (major !== AEROGPU_ABI_MAJOR) {
    throw new AerogpuAbiError(`Unsupported major ABI version: ${major}`);
  }

  return { major, minor };
}

/* -------------------------------- PCI IDs -------------------------------- */

export const AEROGPU_PCI_VENDOR_ID = 0xa3a0;
export const AEROGPU_PCI_DEVICE_ID = 0x0001;
export const AEROGPU_PCI_SUBSYSTEM_VENDOR_ID = AEROGPU_PCI_VENDOR_ID;
export const AEROGPU_PCI_SUBSYSTEM_ID = 0x0001;

export const AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER = 0x03;
export const AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE = 0x00;
export const AEROGPU_PCI_PROG_IF = 0x00;

export const AEROGPU_PCI_BAR0_INDEX = 0;
export const AEROGPU_PCI_BAR0_SIZE_BYTES = 64 * 1024;

export const AEROGPU_PCI_BAR1_INDEX = 1;
export const AEROGPU_PCI_BAR1_SIZE_BYTES = 64 * 1024 * 1024;

/**
 * Offset within BAR1/VRAM where the VBE linear framebuffer (LFB) begins.
 *
 * The canonical BAR1 VRAM layout reserves the first 256KiB for legacy VGA planar storage
 * (4 × 64KiB planes) and places the packed-pixel VBE framebuffer after that region.
 */
export const AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES = 0x40_000;

/* ------------------------------ MMIO registers ---------------------------- */

export const AEROGPU_MMIO_REG_MAGIC = 0x0000;
export const AEROGPU_MMIO_REG_ABI_VERSION = 0x0004;
export const AEROGPU_MMIO_REG_FEATURES_LO = 0x0008;
export const AEROGPU_MMIO_REG_FEATURES_HI = 0x000c;

export const AEROGPU_MMIO_MAGIC = 0x55504741;

export const AEROGPU_FEATURE_FENCE_PAGE = 1n << 0n;
export const AEROGPU_FEATURE_CURSOR = 1n << 1n;
export const AEROGPU_FEATURE_SCANOUT = 1n << 2n;
export const AEROGPU_FEATURE_VBLANK = 1n << 3n;
export const AEROGPU_FEATURE_TRANSFER = 1n << 4n;
export const AEROGPU_FEATURE_ERROR_INFO = 1n << 5n;

export const AEROGPU_MMIO_REG_RING_GPA_LO = 0x0100;
export const AEROGPU_MMIO_REG_RING_GPA_HI = 0x0104;
export const AEROGPU_MMIO_REG_RING_SIZE_BYTES = 0x0108;
export const AEROGPU_MMIO_REG_RING_CONTROL = 0x010c;

export const AEROGPU_RING_CONTROL_ENABLE = 1 << 0;
export const AEROGPU_RING_CONTROL_RESET = 1 << 1;

export const AEROGPU_MMIO_REG_FENCE_GPA_LO = 0x0120;
export const AEROGPU_MMIO_REG_FENCE_GPA_HI = 0x0124;

export const AEROGPU_MMIO_REG_COMPLETED_FENCE_LO = 0x0130;
export const AEROGPU_MMIO_REG_COMPLETED_FENCE_HI = 0x0134;

export const AEROGPU_MMIO_REG_DOORBELL = 0x0200;

export const AEROGPU_MMIO_REG_IRQ_STATUS = 0x0300;
export const AEROGPU_MMIO_REG_IRQ_ENABLE = 0x0304;
export const AEROGPU_MMIO_REG_IRQ_ACK = 0x0308;

export const AEROGPU_IRQ_FENCE = 1 << 0;
export const AEROGPU_IRQ_SCANOUT_VBLANK = 1 << 1;
// NOTE: avoid `1 << 31` (signed 32-bit) which yields a negative number in JS.
export const AEROGPU_IRQ_ERROR = 0x8000_0000;

// Error reporting (ABI 1.3+).
export const AEROGPU_MMIO_REG_ERROR_CODE = 0x0310;
export const AEROGPU_MMIO_REG_ERROR_FENCE_LO = 0x0314;
export const AEROGPU_MMIO_REG_ERROR_FENCE_HI = 0x0318;
export const AEROGPU_MMIO_REG_ERROR_COUNT = 0x031c;

export const AEROGPU_MMIO_REG_SCANOUT0_ENABLE = 0x0400;
export const AEROGPU_MMIO_REG_SCANOUT0_WIDTH = 0x0404;
export const AEROGPU_MMIO_REG_SCANOUT0_HEIGHT = 0x0408;
export const AEROGPU_MMIO_REG_SCANOUT0_FORMAT = 0x040c;
export const AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES = 0x0410;
export const AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO = 0x0414;
export const AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI = 0x0418;

export const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO = 0x0420;
export const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI = 0x0424;
export const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO = 0x0428;
export const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI = 0x042c;
export const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS = 0x0430;

export const AEROGPU_MMIO_REG_CURSOR_ENABLE = 0x0500;
export const AEROGPU_MMIO_REG_CURSOR_X = 0x0504;
export const AEROGPU_MMIO_REG_CURSOR_Y = 0x0508;
export const AEROGPU_MMIO_REG_CURSOR_HOT_X = 0x050c;
export const AEROGPU_MMIO_REG_CURSOR_HOT_Y = 0x0510;
export const AEROGPU_MMIO_REG_CURSOR_WIDTH = 0x0514;
export const AEROGPU_MMIO_REG_CURSOR_HEIGHT = 0x0518;
export const AEROGPU_MMIO_REG_CURSOR_FORMAT = 0x051c;
export const AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO = 0x0520;
export const AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI = 0x0524;
export const AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES = 0x0528;

/* ---------------------------------- Enums -------------------------------- */

export const AerogpuErrorCode = {
  None: 0,
  CmdDecode: 1,
  Oob: 2,
  Backend: 3,
  Internal: 0xffff,
} as const;

export type AerogpuErrorCode = (typeof AerogpuErrorCode)[keyof typeof AerogpuErrorCode];

/**
 * Resource / scanout formats.
 *
 * Semantics:
 * - `*X8*` formats (`B8G8R8X8*`, `R8G8B8X8*`) do not carry alpha. The 8-bit "X"
 *   channel is unused; when converting to RGBA for scanout presentation or
 *   cursor blending, consumers must treat alpha as fully opaque (`0xff`) and
 *   ignore the stored "X" byte.
 * - sRGB variants have the same bit/byte layout as their UNORM counterparts;
 *   only the color space interpretation differs (sampling decodes sRGB to
 *   linear, render-target writes/views may encode linear to sRGB). Presenters
 *   must avoid double-applying gamma when handling sRGB formats.
 */
export const AerogpuFormat = {
  Invalid: 0,
  B8G8R8A8Unorm: 1,
  B8G8R8X8Unorm: 2,
  R8G8B8A8Unorm: 3,
  R8G8B8X8Unorm: 4,
  B5G6R5Unorm: 5,
  B5G5R5A1Unorm: 6,
  B8G8R8A8UnormSrgb: 7,
  B8G8R8X8UnormSrgb: 8,
  R8G8B8A8UnormSrgb: 9,
  R8G8B8X8UnormSrgb: 10,
  D24UnormS8Uint: 32,
  D32Float: 33,
  BC1RgbaUnorm: 64,
  BC1RgbaUnormSrgb: 65,
  BC2RgbaUnorm: 66,
  BC2RgbaUnormSrgb: 67,
  BC3RgbaUnorm: 68,
  BC3RgbaUnormSrgb: 69,
  BC7RgbaUnorm: 70,
  BC7RgbaUnormSrgb: 71,
} as const;

export type AerogpuFormat = (typeof AerogpuFormat)[keyof typeof AerogpuFormat];

const AEROGPU_FORMAT_NAME_BY_VALUE: Record<number, string> = (() => {
  const out: Record<number, string> = {};
  for (const [name, value] of Object.entries(AerogpuFormat)) {
    if (typeof value !== "number") continue;
    out[value >>> 0] = name;
  }
  return out;
})();

function normalizeU32Like(value: number): number | null {
  if (!Number.isFinite(value)) return null;
  if (!Number.isInteger(value)) return null;
  // Accept signed 32-bit integers (e.g. values read from `Int32Array`) and raw unsigned values.
  // Reject numbers outside the u32 domain to avoid silent JS `>>> 0` wrapping.
  if (value < -0x8000_0000 || value > 0xffff_ffff) return null;
  return value >>> 0;
}

/**
 * Best-effort mapping from `AerogpuFormat` discriminant -> enum variant name.
 *
 * Returns `null` for unknown/invalid values.
 */
export function aerogpuFormatName(format: number): string | null {
  const u32 = normalizeU32Like(format);
  if (u32 === null) return null;
  return AEROGPU_FORMAT_NAME_BY_VALUE[u32] ?? null;
}

/**
 * Debug-friendly string for an `AerogpuFormat` value.
 *
 * Examples:
 * - `aerogpuFormatToString(AerogpuFormat.B8G8R8X8Unorm)` => `"B8G8R8X8Unorm (2)"`
 * - `aerogpuFormatToString(1234)` => `"1234"`
 */
export function aerogpuFormatToString(format: number): string {
  if (!Number.isFinite(format)) return "n/a";
  const u32 = normalizeU32Like(format);
  if (u32 === null) return String(format);
  const name = AEROGPU_FORMAT_NAME_BY_VALUE[u32];
  return name ? `${name} (${u32})` : String(u32);
}
