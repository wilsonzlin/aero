import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  isGpuWorkerMessageBase,
  type GpuRuntimeOutMessage,
  type GpuRuntimeStatsMessage,
} from "./src/ipc/gpu-protocol";
import {
  FramebufferFormat,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  computeSharedFramebufferLayout,
} from "./src/ipc/shared-layout";
import {
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_LEGACY_VBE_LFB,
  SCANOUT_SOURCE_WDDM,
  publishScanoutState,
  scanoutBasePaddr,
  snapshotScanoutState,
} from "./src/ipc/scanout_state";
import {
  allocateSharedMemorySegments,
  checkSharedMemorySupport,
  createSharedMemoryViews,
  guestPaddrToRamOffset,
  type GuestRamLayout,
} from "./src/runtime/shared_layout";
import type { WorkerInitMessage } from "./src/runtime/protocol";

function $(id: string): HTMLElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing element #${id}`);
  return el;
}

function setText(id: string, text: string) {
  $(id).textContent = text;
}

function alignUp(value: number, align: number): number {
  if (align <= 0) return value;
  return Math.ceil(value / align) * align;
}

function rgbaQuadrants(width: number, height: number, marker: { x: number; y: number; size: number; rgba: [number, number, number, number] }) {
  const out = new Uint8Array(width * height * 4);
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const top = y < halfH;
      const left = x < halfW;
      let r = 0;
      let g = 0;
      let b = 0;
      if (top && left) {
        r = 255;
      } else if (top && !left) {
        g = 255;
      } else if (!top && left) {
        b = 255;
      } else {
        r = 255;
        g = 255;
        b = 255;
      }
      out[i + 0] = r;
      out[i + 1] = g;
      out[i + 2] = b;
      out[i + 3] = 255;
    }
  }

  // Small marker so source/page-flip changes are obvious while keeping quadrant semantics intact.
  for (let dy = 0; dy < marker.size; dy += 1) {
    const y = marker.y + dy;
    if (y < 0 || y >= height) continue;
    for (let dx = 0; dx < marker.size; dx += 1) {
      const x = marker.x + dx;
      if (x < 0 || x >= width) continue;
      const i = (y * width + x) * 4;
      out[i + 0] = marker.rgba[0];
      out[i + 1] = marker.rgba[1];
      out[i + 2] = marker.rgba[2];
      out[i + 3] = marker.rgba[3];
    }
  }

  return out;
}

function writeBgrxQuadrantsIntoGuest(opts: {
  guestU8: Uint8Array;
  guestLayout: GuestRamLayout;
  basePaddr: number;
  width: number;
  height: number;
  pitchBytes: number;
  xByte: number;
  marker: { x: number; y: number; size: number; bgrx: [number, number, number, number] };
}) {
  const { guestU8, basePaddr, width, height, pitchBytes, xByte } = opts;
  const rowBytes = width * 4;
  if (pitchBytes < rowBytes) throw new Error(`pitchBytes (${pitchBytes}) must be >= width*4 (${rowBytes})`);

  const ramOff = guestPaddrToRamOffset(opts.guestLayout, basePaddr);
  if (ramOff === null) throw new Error(`base_paddr 0x${basePaddr.toString(16)} is not backed by guest RAM`);

  const required = pitchBytes * (height - 1) + rowBytes;
  if (ramOff + required > guestU8.byteLength) {
    throw new Error(`scanout buffer out of bounds (ramOff=0x${ramOff.toString(16)} required=0x${required.toString(16)} guest=0x${guestU8.byteLength.toString(16)})`);
  }

  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  for (let y = 0; y < height; y += 1) {
    const rowBase = ramOff + y * pitchBytes;
    // Fill padding with a pattern so pitch bugs are visually obvious (white streaks).
    guestU8.fill(0xff, rowBase + rowBytes, rowBase + pitchBytes);
    for (let x = 0; x < width; x += 1) {
      const top = y < halfH;
      const left = x < halfW;
      let b = 0;
      let g = 0;
      let r = 0;
      if (top && left) {
        r = 255;
      } else if (top && !left) {
        g = 255;
      } else if (!top && left) {
        b = 255;
      } else {
        r = 255;
        g = 255;
        b = 255;
      }

      const i = rowBase + x * 4;
      guestU8[i + 0] = b;
      guestU8[i + 1] = g;
      guestU8[i + 2] = r;
      guestU8[i + 3] = xByte & 0xff;
    }
  }

  // Marker (in BGRX).
  const marker = opts.marker;
  for (let dy = 0; dy < marker.size; dy += 1) {
    const y = marker.y + dy;
    if (y < 0 || y >= height) continue;
    const rowBase = ramOff + y * pitchBytes;
    for (let dx = 0; dx < marker.size; dx += 1) {
      const x = marker.x + dx;
      if (x < 0 || x >= width) continue;
      const i = rowBase + x * 4;
      guestU8[i + 0] = marker.bgrx[0];
      guestU8[i + 1] = marker.bgrx[1];
      guestU8[i + 2] = marker.bgrx[2];
      guestU8[i + 3] = marker.bgrx[3];
    }
  }
}

async function main() {
  const logEl = $("log");
  const log = (line: string) => {
    logEl.textContent += `${line}\n`;
  };

  const support = checkSharedMemorySupport();
  if (!support.ok) {
    log(`Shared memory unsupported: ${support.reason ?? "unknown"}`);
    return;
  }

  const canvasEl = $("frame");
  if (!(canvasEl instanceof HTMLCanvasElement)) {
    log("Canvas element not found");
    return;
  }
  if (!("transferControlToOffscreen" in canvasEl)) {
    log("OffscreenCanvas is not supported in this browser.");
    return;
  }

  // Pick a width that makes (width*4) NOT already 256-byte aligned so "padded pitch" is meaningful.
  const WIDTH = 257;
  const HEIGHT = 257;

  const dpr = 1;
  canvasEl.width = WIDTH * dpr;
  canvasEl.height = HEIGHT * dpr;
  canvasEl.style.width = `${Math.min(640, WIDTH * 2)}px`;
  canvasEl.style.height = `${Math.min(640, HEIGHT * 2)}px`;

  // Keep the allocation small so the page loads quickly (the wasm32 runtime always
  // reserves a fixed 128MiB region, so allocating a huge guest_size here is wasted).
  //
  // We also disable VRAM for this diagnostic page: the goal is to validate scanout
  // state + pitch + BGRX->RGBA alpha semantics without needing BAR1-backed surfaces.
  const baseSegments = allocateSharedMemorySegments({ guestRamMiB: 8, vramMiB: 0 });

  // Small shared framebuffer used only for the legacy path in this diagnostic page.
  const strideBytes = WIDTH * 4;
  const fbLayout = computeSharedFramebufferLayout(WIDTH, HEIGHT, strideBytes, FramebufferFormat.RGBA8, 0);
  const sharedFramebuffer = new SharedArrayBuffer(fbLayout.totalBytes);
  const sharedFramebufferOffsetBytes = 0;
  const fbHeader = new Int32Array(sharedFramebuffer, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.WIDTH, fbLayout.width);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.HEIGHT, fbLayout.height);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.STRIDE_BYTES, fbLayout.strideBytes);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FORMAT, fbLayout.format);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILE_SIZE, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILES_X, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILES_Y, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
  Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FLAGS, 0);

  const fbSlot0 = new Uint8Array(sharedFramebuffer, fbLayout.framebufferOffsets[0], strideBytes * HEIGHT);
  const fbSlot1 = new Uint8Array(sharedFramebuffer, fbLayout.framebufferOffsets[1], strideBytes * HEIGHT);
  let fbActiveIndex = 0;

  const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
  const frameState = new Int32Array(sharedFrameState);
  Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
  Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

  const segments = {
    ...baseSegments,
    sharedFramebuffer,
    sharedFramebufferOffsetBytes,
  };
  const views = createSharedMemoryViews(segments as any);
  const scanoutWords = views.scanoutStateI32;
  if (!scanoutWords) {
    log("scanoutState view missing (unexpected; shared_layout.allocateSharedMemorySegments should provide it)");
    return;
  }

  // Fill the legacy shared framebuffer with a quadrant pattern + marker.
  const legacyRgba = rgbaQuadrants(WIDTH, HEIGHT, { x: 0, y: 0, size: 12, rgba: [0, 0, 0, 255] });
  const publishLegacyFrame = () => {
    const back = fbActiveIndex ^ 1;
    const dst = back === 0 ? fbSlot0 : fbSlot1;
    dst.set(legacyRgba);
    const seq = (Atomics.load(fbHeader, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
    Atomics.store(
      fbHeader,
      back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
      seq,
    );
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_SEQ, seq);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
    fbActiveIndex = back;
  };
  publishLegacyFrame();

  // WDDM scanout buffers (B8G8R8X8 in guest RAM).
  const rowBytes = WIDTH * 4;
  const paddedPitch = alignUp(rowBytes, 256);
  const maxPitch = Math.max(rowBytes, paddedPitch);
  const requiredMaxBytes = maxPitch * (HEIGHT - 1) + rowBytes;

  // Keep this clear of the demo shared framebuffer offsets (0x20_0000) so this page
  // stays compatible with other harnesses that embed a framebuffer into guest RAM.
  const BUF0_PADDR = 0x0010_0000;
  const BUF1_PADDR = BUF0_PADDR + alignUp(requiredMaxBytes, 0x1000) + 0x1000;

  const applyWddmBuffers = (pitchBytes: number, xByte: number) => {
    writeBgrxQuadrantsIntoGuest({
      guestU8: views.guestU8,
      guestLayout: views.guestLayout,
      basePaddr: BUF0_PADDR,
      width: WIDTH,
      height: HEIGHT,
      pitchBytes,
      xByte,
      marker: { x: 0, y: 0, size: 12, bgrx: [255, 0, 255, xByte & 0xff] }, // magenta marker
    });
    writeBgrxQuadrantsIntoGuest({
      guestU8: views.guestU8,
      guestLayout: views.guestLayout,
      basePaddr: BUF1_PADDR,
      width: WIDTH,
      height: HEIGHT,
      pitchBytes,
      xByte,
      marker: { x: 0, y: 0, size: 12, bgrx: [255, 255, 0, xByte & 0xff] }, // cyan marker
    });
  };

  // UI state.
  type SourceMode = "legacy" | "wddm";
  type PitchMode = "tight" | "padded";
  const ui = {
    source: $("source") as HTMLSelectElement,
    buffer: $("buffer") as HTMLSelectElement,
    pitch: $("pitch") as HTMLSelectElement,
    xbyte: $("xbyte") as HTMLSelectElement,
  };

  const parseSource = (): SourceMode => (ui.source.value === "legacy" ? "legacy" : "wddm");
  const parseBufferIndex = (): number => (ui.buffer.value === "1" ? 1 : 0);
  const parsePitchMode = (): PitchMode => (ui.pitch.value === "tight" ? "tight" : "padded");
  const parseXByte = (): number => (ui.xbyte.value === "255" ? 0xff : 0x00);

  const publishScanoutForUi = () => {
    const source = parseSource();
    const bufferIndex = parseBufferIndex();
    const pitchMode = parsePitchMode();
    const xByte = parseXByte();
    const pitchBytes = pitchMode === "tight" ? rowBytes : paddedPitch;

    // Always keep the guest buffers up to date (even if legacy is selected) so switching
    // sources is instant.
    applyWddmBuffers(pitchBytes, xByte);

    if (source === "legacy") {
      publishScanoutState(scanoutWords, {
        source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
        basePaddrLo: 0,
        basePaddrHi: 0,
        width: 0,
        height: 0,
        pitchBytes: 0,
        format: SCANOUT_FORMAT_B8G8R8X8,
      });
      return;
    }

    const basePaddr = bufferIndex === 0 ? BUF0_PADDR : BUF1_PADDR;
    const base = BigInt(basePaddr >>> 0);
    publishScanoutState(scanoutWords, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: Number(base & 0xffff_ffffn) >>> 0,
      basePaddrHi: Number((base >> 32n) & 0xffff_ffffn) >>> 0,
      width: WIDTH,
      height: HEIGHT,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });
  };

  for (const el of [ui.source, ui.buffer, ui.pitch, ui.xbyte]) {
    el.addEventListener("change", () => {
      try {
        publishScanoutForUi();
      } catch (err) {
        log(err instanceof Error ? err.message : String(err));
      }
    });
  }

  // Seed initial state.
  publishScanoutForUi();

  // Spawn + init the canonical GPU worker.
  const worker = new Worker(new URL("./src/workers/gpu.worker.ts", import.meta.url), { type: "module" });

  const offscreen = canvasEl.transferControlToOffscreen();
  const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

  let readyResolve: (() => void) | null = null;
  let readyReject: ((err: unknown) => void) | null = null;
  const ready = new Promise<void>((resolve, reject) => {
    readyResolve = resolve;
    readyReject = reject;
  });

  let lastStats: GpuRuntimeStatsMessage | null = null;

  worker.addEventListener("message", (event: MessageEvent<unknown>) => {
    const msg = event.data;
    if (!isGpuWorkerMessageBase(msg) || typeof (msg as { type?: unknown }).type !== "string") return;
    const typed = msg as GpuRuntimeOutMessage;
    switch (typed.type) {
      case "ready":
        log(`gpu-worker ready backend=${typed.backendKind}`);
        readyResolve?.();
        readyResolve = null;
        readyReject = null;
        break;
      case "stats":
        lastStats = typed;
        break;
      case "events":
        for (const ev of typed.events) {
          log(`gpu_event ${ev.severity} ${ev.category}: ${ev.message}`);
        }
        break;
      case "error":
        log(`gpu_error ${typed.code ? `${typed.code}: ` : ""}${typed.message}`);
        break;
      default:
        break;
    }
  });

  worker.addEventListener("error", (event) => {
    const err = (event as ErrorEvent).error ?? event;
    readyReject?.(err);
    readyResolve = null;
    readyReject = null;
    log(`worker error: ${err instanceof Error ? err.message : String(err)}`);
  });

  // NOTE: Message ordering matters.
  //
  // The GPU-protocol init path resets some runtime-worker pointers (including scanoutState)
  // for compatibility with harnesses that do not send the runtime-worker init message. Send
  // the GPU-protocol init first, then the runtime-worker init (which plumbs scanoutState +
  // guest RAM) so WDDM scanout presentation is active for this page.
  worker.postMessage(
    {
      ...GPU_MESSAGE_BASE,
      type: "init",
      canvas: offscreen,
      sharedFrameState,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes,
      options: {
        forceBackend: "webgl2_raw",
        disableWebGpu: true,
        outputWidth: WIDTH,
        outputHeight: HEIGHT,
        dpr,
        // Enable a canvas alpha channel so any incorrect XRGB->alpha handling is
        // visible against the checkered background.
        presenter: { canvasAlpha: true },
      },
    },
    [offscreen],
  );

  worker.postMessage({
    kind: "init",
    role: "gpu",
    controlSab: segments.control,
    guestMemory: segments.guestMemory,
    scanoutState: segments.scanoutState,
    scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    ioIpcSab: segments.ioIpc,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
    frameStateSab: sharedFrameState,
  } satisfies WorkerInitMessage);

  await ready;

  // Update debug text periodically.
  const updateText = () => {
    try {
      const snap = snapshotScanoutState(scanoutWords);
      const base = scanoutBasePaddr(snap);
      setText(
        "scanout",
        JSON.stringify(
          {
            generation: snap.generation >>> 0,
            source: snap.source >>> 0,
            base_paddr: `0x${base.toString(16)}`,
            width: snap.width >>> 0,
            height: snap.height >>> 0,
            pitchBytes: snap.pitchBytes >>> 0,
            format: snap.format >>> 0,
          },
          null,
          2,
        ),
      );
    } catch (err) {
      setText("scanout", `failed to snapshot scanoutState: ${err instanceof Error ? err.message : String(err)}`);
    }

    if (lastStats) {
      const safe = JSON.stringify(lastStats, (_k, v) => (typeof v === "bigint" ? v.toString() : v), 2);
      setText("gpuStats", safe);
    }
  };
  updateText();
  const infoTimer = window.setInterval(updateText, 250);
  (infoTimer as unknown as { unref?: () => void }).unref?.();

  // Continuous tick loop:
  //
  // - Forces the worker to re-sample scanout memory continuously so pitch/base_paddr changes show
  //   up immediately.
  // - Avoids stomping the shared frame state while a present is in-flight.
  const tick = (frameTimeMs: number) => {
    const st = Atomics.load(frameState, FRAME_STATUS_INDEX);
    if (st !== FRAME_PRESENTING) {
      if (st === FRAME_PRESENTED) {
        Atomics.store(frameState, FRAME_SEQ_INDEX, (Atomics.load(frameState, FRAME_SEQ_INDEX) + 1) | 0);
        Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
      }
      worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs });
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

void main().catch((err) => {
  const message = err instanceof Error ? err.message : String(err);
  try {
    setText("log", message);
  } catch {
    // Ignore.
  }
});
