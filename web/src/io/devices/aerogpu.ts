import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { PciBar, PciDevice } from "../bus/pci.ts";
import type { TickableDevice } from "../device_manager.ts";
import { guestPaddrToRamOffset, guestRangeInBounds, type GuestRamLayout } from "../../runtime/shared_layout.ts";
import {
  CursorStateIndex,
  publishCursorState,
  type CursorStateUpdate,
} from "../../ipc/cursor_state.ts";
import {
  AEROGPU_ABI_VERSION_U32,
  AEROGPU_FEATURE_CURSOR,
  AEROGPU_FEATURE_FENCE_PAGE,
  AEROGPU_FEATURE_SCANOUT,
  AEROGPU_FEATURE_TRANSFER,
  AEROGPU_FEATURE_VBLANK,
  AEROGPU_MMIO_MAGIC,
  AEROGPU_MMIO_REG_ABI_VERSION,
  AEROGPU_MMIO_REG_CURSOR_ENABLE,
  AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI,
  AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO,
  AEROGPU_MMIO_REG_CURSOR_FORMAT,
  AEROGPU_MMIO_REG_CURSOR_HEIGHT,
  AEROGPU_MMIO_REG_CURSOR_HOT_X,
  AEROGPU_MMIO_REG_CURSOR_HOT_Y,
  AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES,
  AEROGPU_MMIO_REG_CURSOR_WIDTH,
  AEROGPU_MMIO_REG_CURSOR_X,
  AEROGPU_MMIO_REG_CURSOR_Y,
  AEROGPU_MMIO_REG_FEATURES_HI,
  AEROGPU_MMIO_REG_FEATURES_LO,
  AEROGPU_MMIO_REG_MAGIC,
  AEROGPU_PCI_BAR0_SIZE_BYTES,
  AEROGPU_PCI_DEVICE_ID,
  AEROGPU_PCI_VENDOR_ID,
  AerogpuFormat,
} from "../../../../emulator/protocol/aerogpu/aerogpu_pci.ts";

export type AeroGpuCursorSink = {
  setImage(width: number, height: number, rgba8: ArrayBuffer): void;
  setState(enabled: boolean, x: number, y: number, hotX: number, hotY: number): void;
};

const BYTES_PER_PIXEL = 4;
const MAX_CURSOR_DIM = 256;
// Cursor images are small but can still be tens/hundreds of KiB; avoid hashing every 8ms tick.
const CURSOR_IMAGE_POLL_INTERVAL_MS = 64;

const MAX_SAFE_U64_AS_NUMBER = BigInt(Number.MAX_SAFE_INTEGER);

const FNV1A_INIT = 0x811c9dc5;
const FNV1A_PRIME = 0x01000193;

function fnv1aUpdate(h: number, byte: number): number {
  // Use Math.imul for correct 32-bit wraparound.
  return Math.imul(h ^ (byte & 0xff), FNV1A_PRIME) >>> 0;
}

type CursorImagePlan = {
  width: number;
  height: number;
  pitchBytes: number;
  rowBytes: number;
  format: number;
  baseOffset: number;
  source: "ram" | "vram";
  key: string;
};

function cursorFormatKey(format: number): string | null {
  // Match `crates/emulator/src/devices/aerogpu_scanout.rs` cursor handling.
  switch (format >>> 0) {
    case AerogpuFormat.B8G8R8A8Unorm:
    case AerogpuFormat.B8G8R8A8UnormSrgb:
      return "bgra";
    case AerogpuFormat.B8G8R8X8Unorm:
    case AerogpuFormat.B8G8R8X8UnormSrgb:
      return "bgrx";
    case AerogpuFormat.R8G8B8A8Unorm:
    case AerogpuFormat.R8G8B8A8UnormSrgb:
      return "rgba";
    case AerogpuFormat.R8G8B8X8Unorm:
    case AerogpuFormat.R8G8B8X8UnormSrgb:
      return "rgbx";
    default:
      return null;
  }
}

/**
 * Minimal AeroGPU PCI device (BAR0 MMIO register file) with cursor overlay forwarding.
 *
 * This is **not** a complete AeroGPU implementation; it exists to surface the guest-programmed
 * hardware cursor registers to the browser runtime's cursor overlay channel.
 */
export class AeroGpuPciDevice implements PciDevice, TickableDevice {
  readonly name = "aerogpu";
  readonly vendorId = AEROGPU_PCI_VENDOR_ID;
  readonly deviceId = AEROGPU_PCI_DEVICE_ID;
  readonly subsystemVendorId = AEROGPU_PCI_VENDOR_ID;
  readonly subsystemId = AEROGPU_PCI_DEVICE_ID;
  // VGA compatible display controller: base class 0x03, subclass 0x00, progIF 0x00.
  readonly classCode = 0x03_00_00;
  // Keep the canonical AeroGPU BDF stable for deterministic guest enumeration and driver binding.
  //
  // See `docs/pci-device-compatibility.md` / `docs/abi/aerogpu-pci-identity.md`.
  readonly bdf = { bus: 0, device: 7, function: 0 };
  readonly interruptPin = 1 as const; // INTA#

  readonly bars: ReadonlyArray<PciBar | null> = [{ kind: "mmio32", size: AEROGPU_PCI_BAR0_SIZE_BYTES }, null, null, null, null, null];

  readonly #guestU8: Uint8Array;
  readonly #guestLayout: GuestRamLayout;
  readonly #sink: AeroGpuCursorSink | null;
  readonly #cursorStateWords: Int32Array | null;
  readonly #vramU8: Uint8Array | null;
  readonly #vramBasePaddr: number;
  readonly #vramSizeBytes: number;

  // Cursor register file.
  #cursorEnable = false;
  #cursorX = 0;
  #cursorY = 0;
  #cursorHotX = 0;
  #cursorHotY = 0;
  #cursorWidth = 0;
  #cursorHeight = 0;
  #cursorFormat: number = AerogpuFormat.Invalid;
  #cursorPitchBytes = 0;
  #cursorFbGpaLo = 0;
  #cursorFbGpaHi = 0;

  // Forwarding state.
  #forwardingActive = false;
  #stateDirty = false;
  #imageParamsDirty = false;
  #lastImagePollMs = 0;

  #lastSentEnabled: boolean | null = null;
  #lastSentX = 0;
  #lastSentY = 0;
  #lastSentHotX = 0;
  #lastSentHotY = 0;

  #lastSentImageKey: string | null = null;
  #lastSentImageHash = 0;

  constructor(opts: {
    guestU8: Uint8Array;
    guestLayout: GuestRamLayout;
    /**
     * Preferred output: shared CursorState words (SharedArrayBuffer-backed Int32Array).
     *
     * When provided, the device publishes cursor descriptors here and does *not* post legacy
     * cursor.set_image/state messages.
     */
    cursorStateWords?: Int32Array;
    /**
     * Legacy sink fallback used when `cursorStateWords` is unavailable.
     */
    sink?: AeroGpuCursorSink;
    /**
     * Optional shared VRAM aperture (BAR1 backing).
     *
     * When provided, cursor surfaces whose `fbGpa` lives in `[vramBasePaddr, vramBasePaddr + vramSizeBytes)`
     * can be hashed/converted for legacy cursor forwarding and generation bumping.
     */
    vramU8?: Uint8Array;
    vramBasePaddr?: number;
    vramSizeBytes?: number;
  }) {
    this.#guestU8 = opts.guestU8;
    this.#guestLayout = opts.guestLayout;
    this.#cursorStateWords = opts.cursorStateWords ?? null;
    this.#sink = opts.sink ?? null;
    const vram = opts.vramU8 ?? null;
    this.#vramU8 = vram;
    this.#vramBasePaddr = (opts.vramBasePaddr ?? 0) >>> 0;
    const reportedVramSizeBytes = (opts.vramSizeBytes ?? (vram?.byteLength ?? 0)) >>> 0;
    this.#vramSizeBytes = vram ? Math.min(reportedVramSizeBytes, vram.byteLength) >>> 0 : 0;
  }

  mmioRead(barIndex: number, offset: bigint, size: number): number {
    if (barIndex !== 0) return defaultReadValue(size);
    if (size !== 4) return defaultReadValue(size);
    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > AEROGPU_PCI_BAR0_SIZE_BYTES) return defaultReadValue(size);

    switch (off >>> 0) {
      case AEROGPU_MMIO_REG_MAGIC:
        return AEROGPU_MMIO_MAGIC >>> 0;
      case AEROGPU_MMIO_REG_ABI_VERSION:
        return AEROGPU_ABI_VERSION_U32 >>> 0;
      case AEROGPU_MMIO_REG_FEATURES_LO: {
        const features =
          AEROGPU_FEATURE_FENCE_PAGE | AEROGPU_FEATURE_CURSOR | AEROGPU_FEATURE_SCANOUT | AEROGPU_FEATURE_VBLANK | AEROGPU_FEATURE_TRANSFER;
        return Number(features & 0xffff_ffffn) >>> 0;
      }
      case AEROGPU_MMIO_REG_FEATURES_HI: {
        const features =
          AEROGPU_FEATURE_FENCE_PAGE | AEROGPU_FEATURE_CURSOR | AEROGPU_FEATURE_SCANOUT | AEROGPU_FEATURE_VBLANK | AEROGPU_FEATURE_TRANSFER;
        return Number((features >> 32n) & 0xffff_ffffn) >>> 0;
      }

      case AEROGPU_MMIO_REG_CURSOR_ENABLE:
        return this.#cursorEnable ? 1 : 0;
      case AEROGPU_MMIO_REG_CURSOR_X:
        return this.#cursorX >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_Y:
        return this.#cursorY >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_HOT_X:
        return this.#cursorHotX >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_HOT_Y:
        return this.#cursorHotY >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_WIDTH:
        return this.#cursorWidth >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_HEIGHT:
        return this.#cursorHeight >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_FORMAT:
        return this.#cursorFormat >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO:
        return this.#cursorFbGpaLo >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI:
        return this.#cursorFbGpaHi >>> 0;
      case AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES:
        return this.#cursorPitchBytes >>> 0;

      default:
        return 0;
    }
  }

  mmioWrite(barIndex: number, offset: bigint, size: number, value: number): void {
    if (barIndex !== 0) return;
    if (size !== 4) return;
    const off = Number(offset);
    if (!Number.isFinite(off) || off < 0 || off + size > AEROGPU_PCI_BAR0_SIZE_BYTES) return;

    const v = value >>> 0;
    switch (off >>> 0) {
      case AEROGPU_MMIO_REG_CURSOR_ENABLE: {
        const next = v !== 0;
        if (next !== this.#cursorEnable) {
          this.#cursorEnable = next;
          this.#stateDirty = true;
          // Enabling should force an image refresh so the overlay has pixels.
          if (next) this.#imageParamsDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_X: {
        const next = v | 0; // i32
        if (next !== this.#cursorX) {
          this.#cursorX = next;
          this.#stateDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_Y: {
        const next = v | 0; // i32
        if (next !== this.#cursorY) {
          this.#cursorY = next;
          this.#stateDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_HOT_X: {
        if (v !== this.#cursorHotX) {
          this.#cursorHotX = v;
          this.#stateDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_HOT_Y: {
        if (v !== this.#cursorHotY) {
          this.#cursorHotY = v;
          this.#stateDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_WIDTH: {
        if (v !== this.#cursorWidth) {
          this.#cursorWidth = v;
          this.#imageParamsDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_HEIGHT: {
        if (v !== this.#cursorHeight) {
          this.#cursorHeight = v;
          this.#imageParamsDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_FORMAT: {
        if (v !== (this.#cursorFormat >>> 0)) {
          this.#cursorFormat = v;
          this.#imageParamsDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO: {
        if (v !== this.#cursorFbGpaLo) {
          this.#cursorFbGpaLo = v;
          this.#imageParamsDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI: {
        if (v !== this.#cursorFbGpaHi) {
          this.#cursorFbGpaHi = v;
          this.#imageParamsDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      case AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES: {
        if (v !== this.#cursorPitchBytes) {
          this.#cursorPitchBytes = v;
          this.#imageParamsDirty = true;
        }
        this.#forwardingActive = true;
        return;
      }
      default:
        return;
    }
  }

  tick(nowMs: number): void {
    // Do not interfere with cursorDemo or other synthetic cursor producers unless the guest (or a harness)
    // has actually touched the hardware cursor registers.
    if (!this.#forwardingActive) return;

    let plan: CursorImagePlan | null = null;
    let renderEnabled = false;
    try {
      // Best-effort policy: only render cursor when both:
      // - CURSOR_ENABLE is set, and
      // - we can safely read/convert the cursor bitmap.
      plan = this.#computeCursorImagePlan();
      renderEnabled = this.#cursorEnable && plan !== null;

      // Shared CursorState path: publish image parameter changes even if the cursor is disabled or
      // invalid so consumers can observe the guest-programmed registers (and so the GPU worker can
      // treat the hardware cursor path as authoritative once the guest starts touching it).
      if (this.#cursorStateWords && this.#imageParamsDirty) {
        const words = this.#cursorStateWords;
        // Establish a new baseline hash if we can safely read the image (enabled + valid plan).
        this.#lastSentImageKey = plan?.key ?? null;
        this.#lastSentImageHash = renderEnabled && plan ? this.#hashCursor(plan) : 0;
        this.#lastImagePollMs = nowMs;
        const update = this.#cursorUpdateForPublish(this.#cursorEnable, plan);
        publishCursorState(words, update);
        // The publish wrote dynamic fields too; keep the fast-path caches consistent.
        this.#lastSentEnabled = !!this.#cursorEnable;
        this.#lastSentX = update.x;
        this.#lastSentY = update.y;
        this.#lastSentHotX = update.hotX;
        this.#lastSentHotY = update.hotY;
        this.#stateDirty = false;
        this.#imageParamsDirty = false;
      }

      // Image updates: only when enabled and either parameters changed or the backing bytes changed.
      if (renderEnabled && plan) {
        const shouldPoll = nowMs - this.#lastImagePollMs >= CURSOR_IMAGE_POLL_INTERVAL_MS;
        const needImage = this.#imageParamsDirty || this.#lastSentImageKey !== plan.key;
        if (needImage || shouldPoll) {
          this.#lastImagePollMs = nowMs;
          if (this.#cursorStateWords) {
            // Shared CursorState path: bump generation to signal the GPU worker to re-read the
            // cursor image from guest RAM.
            const didUpdate = this.#maybeBumpCursorStateImage(plan, { force: needImage });
            if (didUpdate) {
              // In the legacy path we delay enabling until after `setImage` completes. In the
              // shared-memory path there is no such ordering guarantee, but publishing an updated
              // descriptor still implies cursor state may need to be refreshed.
              this.#stateDirty = true;
            }
          } else if (this.#sink) {
            if (this.#maybeSendCursorImage(plan, { force: needImage })) {
              // Ensure the presenter sees pixels before we enable the cursor state.
              this.#stateDirty = true;
            }
          }
          this.#imageParamsDirty = false;
        }
      } else {
        // Stop repeatedly attempting invalid/disabled images until parameters change.
        this.#imageParamsDirty = false;
      }
    } catch {
      // Device models should never crash the entire I/O worker. If cursor state becomes invalid (or a
      // guest programs pathological values), fall back to disabling the overlay.
      plan = null;
      renderEnabled = false;
      this.#imageParamsDirty = false;
      this.#stateDirty = true;
    }

    const effectiveEnabled = this.#cursorStateWords ? this.#cursorEnable : renderEnabled;
    if (!this.#stateDirty && this.#lastSentEnabled === effectiveEnabled) {
      // Avoid building objects/doing comparisons on the hot path.
      return;
    }
    if (this.#cursorStateWords) {
      this.#writeCursorStateIfChanged(effectiveEnabled, plan);
      this.#stateDirty = false;
      return;
    }
    const sentOk = this.#sendCursorStateIfChanged(renderEnabled);
    if (sentOk) {
      this.#stateDirty = false;
    }
  }

  debugProgramCursor(opts: {
    enabled: boolean;
    x: number;
    y: number;
    hotX: number;
    hotY: number;
    width: number;
    height: number;
    format: number;
    fbGpa: number;
    pitchBytes: number;
  }): void {
    // Drive the same MMIO write paths the guest would use so forwarding logic stays exercised.
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO), 4, opts.fbGpa >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI), 4, 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES), 4, opts.pitchBytes >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_WIDTH), 4, opts.width >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_HEIGHT), 4, opts.height >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_FORMAT), 4, opts.format >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_HOT_X), 4, opts.hotX >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_HOT_Y), 4, opts.hotY >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_X), 4, opts.x >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_Y), 4, opts.y >>> 0);
    this.mmioWrite(0, BigInt(AEROGPU_MMIO_REG_CURSOR_ENABLE), 4, opts.enabled ? 1 : 0);
  }

  #cursorFbGpa(): bigint {
    return (BigInt(this.#cursorFbGpaHi >>> 0) << 32n) | BigInt(this.#cursorFbGpaLo >>> 0);
  }

  #computeCursorImagePlan(): CursorImagePlan | null {
    const formatKind = cursorFormatKey(this.#cursorFormat >>> 0);
    if (!formatKind) return null;

    const rawW = this.#cursorWidth >>> 0;
    const rawH = this.#cursorHeight >>> 0;
    if (rawW === 0 || rawH === 0) return null;
    const width = Math.min(rawW, MAX_CURSOR_DIM);
    const height = Math.min(rawH, MAX_CURSOR_DIM);

    const pitchBytes = this.#cursorPitchBytes >>> 0;
    const rowBytes = width * BYTES_PER_PIXEL;
    if (pitchBytes < rowBytes) return null;

    const fbGpa64 = this.#cursorFbGpa();
    if (fbGpa64 === 0n) return null;
    if (fbGpa64 > BigInt(Number.MAX_SAFE_INTEGER)) return null;

    // Validate GPA arithmetic does not wrap and that the guest range is backed by RAM.
    //
    // `neededBytes` = pitch*(height-1) + rowBytes (same as Rust cursor validation).
    const neededBytes = BigInt(pitchBytes) * BigInt(height - 1) + BigInt(rowBytes);
    if (neededBytes > MAX_SAFE_U64_AS_NUMBER) return null;
    // Detect u64 wrap (best-effort).
    const endGpa = fbGpa64 + neededBytes;
    if (endGpa < fbGpa64) return null;
    if (endGpa > 0xffff_ffff_ffff_ffffn) return null;

    // Resolve the cursor surface backing store: VRAM aperture or guest RAM.
    let baseOffset: number | null = null;
    // Always either RAM or VRAM when a plan is returned.
    let source: "ram" | "vram" = "ram";
    const vram = this.#vramU8;
    if (vram && this.#vramSizeBytes > 0) {
      const base = BigInt(this.#vramBasePaddr >>> 0);
      const end = base + BigInt(this.#vramSizeBytes >>> 0);
      if (fbGpa64 >= base && endGpa <= end) {
        const offBig = fbGpa64 - base;
        if (offBig <= BigInt(Number.MAX_SAFE_INTEGER)) {
          baseOffset = Number(offBig);
          source = "vram";
        }
      }
    }
    if (baseOffset === null) {
      const fbGpa = Number(fbGpa64);
      try {
        if (!guestRangeInBounds(this.#guestLayout, fbGpa, Number(neededBytes))) return null;
        baseOffset = guestPaddrToRamOffset(this.#guestLayout, fbGpa);
      } catch {
        return null;
      }
      if (baseOffset === null) return null;
    }

    const key = `${fbGpa64.toString(16)}:${pitchBytes}:${width}x${height}:${this.#cursorFormat >>> 0}`;
    return { width, height, pitchBytes, rowBytes, format: this.#cursorFormat >>> 0, baseOffset, source, key };
  }

  #hashCursor(plan: CursorImagePlan): number {
    const src = plan.source === "vram" ? this.#vramU8 : this.#guestU8;
    if (!src) return 0;
    const fmt = cursorFormatKey(plan.format);
    if (!fmt) return 0;

    let h = FNV1A_INIT;
    for (let y = 0; y < plan.height; y += 1) {
      const rowOff = plan.baseOffset + y * plan.pitchBytes;
      for (let x = 0; x < plan.width; x += 1) {
        const i = rowOff + x * 4;
        const b0 = src[i + 0] ?? 0;
        const b1 = src[i + 1] ?? 0;
        const b2 = src[i + 2] ?? 0;
        const b3 = src[i + 3] ?? 0;

        let r = 0;
        let g = 0;
        let b = 0;
        let a = 255;
        if (fmt === "bgra") {
          r = b2;
          g = b1;
          b = b0;
          a = b3;
        } else if (fmt === "bgrx") {
          r = b2;
          g = b1;
          b = b0;
          a = 255;
        } else if (fmt === "rgba") {
          r = b0;
          g = b1;
          b = b2;
          a = b3;
        } else if (fmt === "rgbx") {
          r = b0;
          g = b1;
          b = b2;
          a = 255;
        }

        h = fnv1aUpdate(h, r);
        h = fnv1aUpdate(h, g);
        h = fnv1aUpdate(h, b);
        h = fnv1aUpdate(h, a);
      }
    }

    return h >>> 0;
  }

  #fillCursor(plan: CursorImagePlan, out: Uint8Array): number {
    const src = plan.source === "vram" ? this.#vramU8 : this.#guestU8;
    if (!src) return 0;
    const fmt = cursorFormatKey(plan.format);
    if (!fmt) return 0;

    let h = FNV1A_INIT;
    let dstOff = 0;
    for (let y = 0; y < plan.height; y += 1) {
      const rowOff = plan.baseOffset + y * plan.pitchBytes;
      for (let x = 0; x < plan.width; x += 1) {
        const i = rowOff + x * 4;
        const b0 = src[i + 0] ?? 0;
        const b1 = src[i + 1] ?? 0;
        const b2 = src[i + 2] ?? 0;
        const b3 = src[i + 3] ?? 0;

        let r = 0;
        let g = 0;
        let b = 0;
        let a = 255;
        if (fmt === "bgra") {
          r = b2;
          g = b1;
          b = b0;
          a = b3;
        } else if (fmt === "bgrx") {
          r = b2;
          g = b1;
          b = b0;
          a = 255;
        } else if (fmt === "rgba") {
          r = b0;
          g = b1;
          b = b2;
          a = b3;
        } else if (fmt === "rgbx") {
          r = b0;
          g = b1;
          b = b2;
          a = 255;
        }

        out[dstOff + 0] = r;
        out[dstOff + 1] = g;
        out[dstOff + 2] = b;
        out[dstOff + 3] = a;
        dstOff += 4;

        h = fnv1aUpdate(h, r);
        h = fnv1aUpdate(h, g);
        h = fnv1aUpdate(h, b);
        h = fnv1aUpdate(h, a);
      }
    }

    return h >>> 0;
  }

  #maybeSendCursorImage(plan: CursorImagePlan, opts: { force: boolean }): boolean {
    const sink = this.#sink;
    if (!sink) return false;
    const keyChanged = this.#lastSentImageKey !== plan.key;
    if (!opts.force && !keyChanged) {
      const nextHash = this.#hashCursor(plan);
      if (nextHash === this.#lastSentImageHash) {
        return false;
      }
    }

    const out = new Uint8Array(plan.width * plan.height * 4);
    const hash = this.#fillCursor(plan, out);
    if (!opts.force && !keyChanged && hash === this.#lastSentImageHash) {
      return false;
    }
    try {
      sink.setImage(plan.width, plan.height, out.buffer);
    } catch {
      // Best-effort: cursor forwarding should never crash the IO worker. If posting fails (e.g. worker shutdown),
      // keep the previous sent key/hash so we'll retry on the next poll when possible.
      return false;
    }

    this.#lastSentImageKey = plan.key;
    this.#lastSentImageHash = hash;
    return true;
  }

  #cursorUpdateForPublish(enabled: boolean, plan: CursorImagePlan | null): CursorStateUpdate {
    const fbGpa64 = this.#cursorFbGpa();
    const width = plan ? plan.width : Math.min(this.#cursorWidth >>> 0, MAX_CURSOR_DIM);
    const height = plan ? plan.height : Math.min(this.#cursorHeight >>> 0, MAX_CURSOR_DIM);
    const format = plan ? plan.format : (this.#cursorFormat >>> 0);
    const pitchBytes = plan ? plan.pitchBytes : (this.#cursorPitchBytes >>> 0);
    return {
      enable: enabled ? 1 : 0,
      x: this.#cursorX | 0,
      y: this.#cursorY | 0,
      hotX: this.#cursorHotX >>> 0,
      hotY: this.#cursorHotY >>> 0,
      width,
      height,
      pitchBytes,
      format,
      basePaddrLo: Number(fbGpa64 & 0xffff_ffffn) >>> 0,
      basePaddrHi: Number((fbGpa64 >> 32n) & 0xffff_ffffn) >>> 0,
    };
  }

  #maybeBumpCursorStateImage(plan: CursorImagePlan, opts: { force: boolean }): boolean {
    const words = this.#cursorStateWords;
    if (!words) return false;
    const keyChanged = this.#lastSentImageKey !== plan.key;
    if (!opts.force && !keyChanged) {
      const nextHash = this.#hashCursor(plan);
      if (nextHash === this.#lastSentImageHash) {
        return false;
      }
      // Hash changed: bump generation and update baseline.
      this.#lastSentImageHash = nextHash;
    } else {
      // Forced refresh: update the baseline hash so polling can detect subsequent in-place updates.
      this.#lastSentImageHash = this.#hashCursor(plan);
    }
    this.#lastSentImageKey = plan.key;
    publishCursorState(words, this.#cursorUpdateForPublish(true, plan));
    return true;
  }

  #writeCursorStateIfChanged(enabled: boolean, plan: CursorImagePlan | null): void {
    const words = this.#cursorStateWords;
    if (!words) return;

    const update = this.#cursorUpdateForPublish(enabled, plan);

    if (
      this.#lastSentEnabled === !!enabled &&
      this.#lastSentX === update.x &&
      this.#lastSentY === update.y &&
      this.#lastSentHotX === update.hotX &&
      this.#lastSentHotY === update.hotY
    ) {
      return;
    }

    this.#lastSentEnabled = !!enabled;
    this.#lastSentX = update.x;
    this.#lastSentY = update.y;
    this.#lastSentHotX = update.hotX;
    this.#lastSentHotY = update.hotY;

    // Note: only update the "dynamic" fields here. Surface parameters and generation are
    // published via `publishCursorState()` when they change (or when we detect in-place updates).
    Atomics.store(words, CursorStateIndex.ENABLE, update.enable | 0);
    Atomics.store(words, CursorStateIndex.X, update.x | 0);
    Atomics.store(words, CursorStateIndex.Y, update.y | 0);
    Atomics.store(words, CursorStateIndex.HOT_X, update.hotX | 0);
    Atomics.store(words, CursorStateIndex.HOT_Y, update.hotY | 0);
  }

  #sendCursorStateIfChanged(enabled: boolean): boolean {
    const sink = this.#sink;
    if (!sink) return false;
    const x = this.#cursorX | 0;
    const y = this.#cursorY | 0;
    const hotX = this.#cursorHotX >>> 0;
    const hotY = this.#cursorHotY >>> 0;

    if (
      this.#lastSentEnabled === enabled &&
      this.#lastSentX === x &&
      this.#lastSentY === y &&
      this.#lastSentHotX === hotX &&
      this.#lastSentHotY === hotY
    ) {
      return true;
    }
    try {
      sink.setState(enabled, x, y, hotX, hotY);
    } catch {
      // Best-effort: do not crash the IO worker if the sink throws (e.g. during shutdown).
      return false;
    }

    this.#lastSentEnabled = enabled;
    this.#lastSentX = x;
    this.#lastSentY = y;
    this.#lastSentHotX = hotX;
    this.#lastSentHotY = hotY;
    return true;
  }
}
