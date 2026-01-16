import {
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
} from "./src/ipc/gpu-protocol";
import {
  FramebufferFormat,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  computeSharedFramebufferLayout,
} from "./src/ipc/shared-layout";
import { publishScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM } from "./src/ipc/scanout_state";
import { VRAM_BASE_PADDR } from "./src/arch/guest_phys.ts";
import type { WorkerInitMessage } from "./src/runtime/protocol";
import { createSharedMemoryViews } from "./src/runtime/shared_layout";
import { allocateHarnessSharedMemorySegments } from "./src/runtime/harness_shared_memory";
import { fnv1a32Hex } from "./src/utils/fnv1a";
import { formatOneLineError } from "./src/text";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      backend?: string;
      error?: string;
      hash?: string;
      expectedHash?: string;
      sourceHash?: string;
      expectedSourceHash?: string;
      pass?: boolean;
      cursorOk?: boolean;
      sharedDirtyCleared?: boolean;
      metrics?: any;
      stats?: any;
      events?: any[];
      samplePixels?: () => Promise<{
        backend: string;
        cursor?: {
          x: number;
          y: number;
          pixel: number[];
          nearby: number[];
        };
        source: {
          width: number;
          height: number;
          topLeft: number[];
          topRight: number[];
          bottomLeft: number[];
          bottomRight: number[];
        };
        presented: {
          width: number;
          height: number;
          topLeft: number[];
          topRight: number[];
          bottomLeft: number[];
          bottomRight: number[];
        };
        scanoutUpdate: {
          x: number;
          y: number;
          before: number[];
          after: number[];
        };
      }>;
      runContextLossRecovery?: () => Promise<{
        ok: boolean;
        error?: string;
        loseOk?: boolean;
        restoreOk?: boolean;
        before?: {
          readyCount: number;
          counters?: any;
        };
        after?: {
          readyCount: number;
          counters?: any;
          hash?: string;
          expectedHash?: string;
          samples?: {
            topLeft: number[];
            topRight: number[];
            bottomLeft: number[];
            bottomRight: number[];
          };
        };
      }>;
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function renderError(message: string): void {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message, pass: false };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function createExpectedTestPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const left = x < halfW;
      const top = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      if (top && left) {
        out[i + 0] = 255;
        out[i + 1] = 0;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (top && !left) {
        out[i + 0] = 0;
        out[i + 1] = 255;
        out[i + 2] = 0;
        out[i + 3] = 255;
      } else if (!top && left) {
        out[i + 0] = 0;
        out[i + 1] = 0;
        out[i + 2] = 255;
        out[i + 3] = 255;
      } else {
        out[i + 0] = 255;
        out[i + 1] = 255;
        out[i + 2] = 255;
        out[i + 3] = 255;
      }
    }
  }

  return out;
}

function writeBgrxTestPattern(dst: Uint8Array, width: number, height: number, pitchBytes: number): void {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const rowBytes = width * 4;
  if (pitchBytes < rowBytes) throw new Error("pitchBytes too small");

  dst.fill(0);

  for (let y = 0; y < height; y += 1) {
    const rowOff = y * pitchBytes;
    for (let x = 0; x < width; x += 1) {
      const pxOff = rowOff + x * 4;
      const left = x < halfW;
      const top = y < halfH;

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

      // B8G8R8X8 (BGRX) in memory, with X intentionally 0 to validate alpha=255 policy.
      dst[pxOff + 0] = b;
      dst[pxOff + 1] = g;
      dst[pxOff + 2] = r;
      dst[pxOff + 3] = 0;
    }
  }
}

async function main(): Promise<void> {
  const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

  const canvas = $("frame");
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError("Canvas element not found");
    return;
  }

  const backendEl = $("backend");
  const status = $("status");
  const log = (line: string) => {
    if (status) status.textContent += `${line}\n`;
  };

  try {
    if (!("transferControlToOffscreen" in canvas)) {
      throw new Error("OffscreenCanvas is not supported in this browser.");
    }

    const width = 64;
    const height = 64;
    const pitchBytes = width * 4 + 16;

    canvas.width = width;
    canvas.height = height;
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;

    const params = new URLSearchParams(window.location.search);
    const backingParam = (params.get("backing") ?? "").toLowerCase();
    const vramParam = (params.get("vram") ?? "").toLowerCase();
    const useVramBacking = backingParam === "vram" || vramParam === "1" || vramParam === "true";

    // Allocate only what this harness needs (guest RAM backing for the scanout surface, plus
    // a shared ScanoutState descriptor). This avoids reserving a large wasm32 runtime region,
    // which is unnecessary for this smoke test.
    // Still provide a minimal legacy shared framebuffer header so the test can assert that
    // WDDM scanout presentation clears any pending legacy dirty flag.
    const fbLayout = computeSharedFramebufferLayout(1, 1, 4, FramebufferFormat.RGBA8, 0);
    const sharedFramebuffer = new SharedArrayBuffer(fbLayout.totalBytes);
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
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILE_SIZE, fbLayout.tileSize);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILES_X, fbLayout.tilesX);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.TILES_Y, fbLayout.tilesY);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, fbLayout.dirtyWordsPerBuffer);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
    Atomics.store(fbHeader, SharedFramebufferHeaderIndex.FLAGS, 0);
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: useVramBacking ? 1 * 1024 * 1024 : 0,
    });
    const views = createSharedMemoryViews(segments);

    if (!views.scanoutStateI32) {
      throw new Error("scanoutState was not allocated");
    }

    const requiredScanoutBytes = pitchBytes * height;
    let backing: Uint8Array;

    let scanoutBasePaddr = 0;
    if (useVramBacking) {
      if (views.vramSizeBytes <= 0) {
        throw new Error("VRAM segment was not allocated (expected vramMiB>0)");
      }
      // Keep a small non-zero offset so the test covers BAR1 address translation math.
      const vramOffset = 0x1000;
      if (vramOffset + requiredScanoutBytes > views.vramSizeBytes) {
        throw new Error("VRAM segment too small for scanout test pattern");
      }
      backing = views.vramU8.subarray(vramOffset, vramOffset + requiredScanoutBytes);
      writeBgrxTestPattern(backing, width, height, pitchBytes);
      scanoutBasePaddr = VRAM_BASE_PADDR + vramOffset;
    } else {
      // Keep this non-zero so scanout translation tests aren't trivially special-cased.
      const basePaddr = 0x1000;
      if (basePaddr + requiredScanoutBytes > views.guestLayout.guest_size) {
        throw new Error("guest RAM too small for scanout test pattern");
      }
      // Write BGRX pixels into guest RAM at base_paddr (with non-tight pitch).
      backing = views.guestU8.subarray(basePaddr, basePaddr + requiredScanoutBytes);
      writeBgrxTestPattern(backing, width, height, pitchBytes);
      scanoutBasePaddr = basePaddr;
    }

    // Publish a WDDM scanout descriptor pointing at the backing surface.
    publishScanoutState(views.scanoutStateI32, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: scanoutBasePaddr >>> 0,
      basePaddrHi: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    // Spawn the canonical GPU worker.
    const worker = new Worker(new URL("./src/workers/gpu.worker.ts", import.meta.url), { type: "module" });

    let fatalError: string | null = null;
    let backendKind: string | null = null;
    let lastMetrics: any | null = null;
    let lastStats: any | null = null;
    const gpuEvents: any[] = [];
    let readyCount = 0;
    let readyResolve!: () => void;
    let readyReject!: (err: unknown) => void;
    const ready = new Promise<void>((resolve, reject) => {
      readyResolve = resolve;
      readyReject = reject;
    });

    let nextRequestId = 1;
    const pendingScreenshot = new Map<number, { resolve: (msg: any) => void; reject: (err: unknown) => void }>();
    const pendingPresentedScreenshot = new Map<number, { resolve: (msg: any) => void; reject: (err: unknown) => void }>();

    worker.addEventListener("message", (event) => {
      const msg = event.data as unknown;
      if (!msg || typeof msg !== "object") return;
      const record = msg as Record<string, unknown>;
      const type = record.type;
      if (typeof type !== "string") return;

      switch (type) {
        case "ready":
          backendKind = String(record.backendKind ?? "unknown");
          readyCount += 1;
          readyResolve();
          break;
        case "screenshot": {
          const requestId = typeof record.requestId === "number" ? record.requestId : Number.NaN;
          const pending = pendingScreenshot.get(requestId);
          if (!pending) break;
          pendingScreenshot.delete(requestId);
          pending.resolve(msg);
          break;
        }
        case "screenshot_presented": {
          const requestId = typeof record.requestId === "number" ? record.requestId : Number.NaN;
          const pending = pendingPresentedScreenshot.get(requestId);
          if (!pending) break;
          pendingPresentedScreenshot.delete(requestId);
          pending.resolve(msg);
          break;
        }
        case "metrics":
          lastMetrics = msg;
          break;
        case "stats":
          lastStats = msg;
          break;
        case "events":
          if (Array.isArray(record.events)) gpuEvents.push(...(record.events as unknown[]));
          break;
        case "error":
          fatalError = String(record.message ?? "unknown worker error");
          readyResolve();
          break;
      }
    });

    worker.addEventListener("error", (event) => {
      readyReject((event as ErrorEvent).error ?? event);
    });

    // Worker init (SharedArrayBuffers + shared guest RAM).
    const initMsg: WorkerInitMessage = {
      kind: "init",
      role: "gpu",
      controlSab: segments.control,
      guestMemory: segments.guestMemory,
      ...(useVramBacking
        ? {
            vram: (() => {
              if (!(segments.vram instanceof SharedArrayBuffer)) {
                throw new Error("VRAM segment missing from WorkerInitMessage");
              }
              return segments.vram;
            })(),
            vramBasePaddr: VRAM_BASE_PADDR,
            vramSizeBytes: views.vramSizeBytes,
          }
        : {}),
      ioIpcSab: segments.ioIpc,
      sharedFramebuffer: segments.sharedFramebuffer,
      sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
      vgaFramebuffer: segments.sharedFramebuffer,
      scanoutState: segments.scanoutState,
      scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    };
    worker.postMessage(initMsg);

    // GPU runtime init (presenter + screenshot API).
    const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
    const frameState = new Int32Array(sharedFrameState);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
    Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

    const offscreen = canvas.transferControlToOffscreen();
    worker.postMessage(
      {
        ...GPU_MESSAGE_BASE,
        type: "init",
        canvas: offscreen,
        sharedFrameState,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        options: {
          // Prefer the raw WebGL2 backend for stability in headless Chromium.
          // The test validates:
          // - presented pixels via `screenshot_presented`
          // - source pixels (including alpha=255 policy) via `screenshot`
          forceBackend: "webgl2_raw",
          disableWebGpu: true,
          outputWidth: width,
          outputHeight: height,
          dpr: 1,
        },
      },
      [offscreen],
    );
    await ready;
    if (fatalError) throw new Error(fatalError);

    const backend = backendKind ?? "unknown";
    if (backendEl) backendEl.textContent = backend;

    // Ensure WDDM scanout presentation also clears any pending legacy shared framebuffer dirty flag.
    // This prevents legacy output from "flashing back" once WDDM owns scanout.
    const sharedHeader = new Int32Array(
      segments.sharedFramebuffer,
      segments.sharedFramebufferOffsetBytes,
      SHARED_FRAMEBUFFER_HEADER_U32_LEN,
    );
    Atomics.store(sharedHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);

    // Trigger a few ticks without ever marking the legacy shared framebuffer DIRTY.
    //
    // This validates that WDDM scanout presentation is not gated on the legacy
    // shared-framebuffer pacing state (FRAME_STATUS=DIRTY), matching the behavior
    // required by the main-thread frame scheduler once scanout is WDDM-owned.

    for (let i = 0; i < 3; i += 1) {
      worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
      await sleep(10);
    }

    // Capture the latest metrics message so tests can assert outputSource/scanout telemetry.
    //
    // The GPU worker only emits metrics while processing ticks, so keep sending ticks until we
    // observe a snapshot that reflects active WDDM scanout (bounded).
    {
      const metricsOk = (m: any): boolean => {
        return !!m && m.outputSource === "wddm_scanout" && m.scanout?.source === SCANOUT_SOURCE_WDDM;
      };
      const deadline = performance.now() + 2000;
      while (!metricsOk(lastMetrics) && performance.now() < deadline) {
        worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
        await sleep(20);
      }
      if (!metricsOk(lastMetrics)) {
        throw new Error(
          `Timed out waiting for WDDM scanout metrics snapshot (got outputSource=${String(lastMetrics?.outputSource ?? "none")} scanout.source=${String(lastMetrics?.scanout?.source ?? "none")})`,
        );
      }
    }

    // Wait briefly for PRESENTED so screenshot readback is stable.
    {
      const deadline = performance.now() + 2000;
      while (Atomics.load(frameState, FRAME_STATUS_INDEX) !== FRAME_PRESENTED && performance.now() < deadline) {
        await sleep(5);
      }
    }

    const sharedDirtyAfter = Atomics.load(sharedHeader, SharedFramebufferHeaderIndex.FRAME_DIRTY) >>> 0;
    const sharedDirtyCleared = sharedDirtyAfter === 0;

    const requestScreenshot = (): Promise<any> => {
      const requestId = nextRequestId++;
      worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId });
      return new Promise((resolve, reject) => {
        pendingScreenshot.set(requestId, { resolve, reject });
        setTimeout(() => {
          const pending = pendingScreenshot.get(requestId);
          if (!pending) return;
          pendingScreenshot.delete(requestId);
          reject(new Error("screenshot request timed out"));
        }, 5000);
      });
    };

    const requestPresentedScreenshot = (includeCursor = false): Promise<any> => {
      const requestId = nextRequestId++;
      worker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot_presented", requestId, includeCursor });
      return new Promise((resolve, reject) => {
        pendingPresentedScreenshot.set(requestId, { resolve, reject });
        setTimeout(() => {
          const pending = pendingPresentedScreenshot.get(requestId);
          if (!pending) return;
          pendingPresentedScreenshot.delete(requestId);
          reject(new Error("screenshot_presented request timed out"));
        }, 5000);
      });
    };

    // Move the cursor once WDDM scanout is active. This exercises the cursor-forwarding and
    // redraw paths and ensures they do not "flash back" to legacy framebuffer output.
    const cursorX = 2;
    const cursorY = 2;
    {
      const cursorRgba8 = new Uint8Array([0, 0, 0, 255]).buffer;
      worker.postMessage({ ...GPU_MESSAGE_BASE, type: "cursor_set_image", width: 1, height: 1, rgba8: cursorRgba8 });
      worker.postMessage({
        ...GPU_MESSAGE_BASE,
        type: "cursor_set_state",
        enabled: true,
        x: cursorX,
        y: cursorY,
        hotX: 0,
        hotY: 0,
      });
      await sleep(25);
    }

    const presentedWithCursorShot = await requestPresentedScreenshot(true);
    // Now capture again with cursor explicitly disabled so we can validate the underlying scanout
    // pixels remain correct.
    const sourceShot = await requestScreenshot();
    const presentedShot = await requestPresentedScreenshot(false);

    const sourceWidth = Number(sourceShot.width) | 0;
    const sourceHeight = Number(sourceShot.height) | 0;
    const sourceRgba8 = new Uint8Array(sourceShot.rgba8);
    const sourceExpected = createExpectedTestPattern(sourceWidth, sourceHeight);
    const sourceHash = fnv1a32Hex(sourceRgba8);
    const expectedSourceHash = fnv1a32Hex(sourceExpected);

    const presentedWidth = Number(presentedShot.width) | 0;
    const presentedHeight = Number(presentedShot.height) | 0;
    const presentedRgba8 = new Uint8Array(presentedShot.rgba8);
    const presentedExpected = createExpectedTestPattern(presentedWidth, presentedHeight);
    const hash = fnv1a32Hex(presentedRgba8);
    const expectedHash = fnv1a32Hex(presentedExpected);

    const presentedWithCursorWidth = Number(presentedWithCursorShot.width) | 0;
    const presentedWithCursorHeight = Number(presentedWithCursorShot.height) | 0;
    const presentedWithCursorRgba8 = new Uint8Array(presentedWithCursorShot.rgba8);
    const cursorPixel = sample(presentedWithCursorRgba8, presentedWithCursorWidth, cursorX, cursorY);
    const cursorNearby = sample(presentedWithCursorRgba8, presentedWithCursorWidth, 8, 8);

    // Preserve the original smoke-test invariant: `pass` tracks scanout correctness only.
    // Cursor overlay checks are asserted separately so failures produce clearer diagnostics.
    const pass = hash === expectedHash && sourceHash === expectedSourceHash;
    const cursorOk = cursorPixel[0] === 0 && cursorPixel[1] === 0 && cursorPixel[2] === 0 && cursorPixel[3] === 255;

    function sample(rgba: Uint8Array, width_: number, x: number, y: number): number[] {
      const i = (y * width_ + x) * 4;
      return [rgba[i + 0] ?? 0, rgba[i + 1] ?? 0, rgba[i + 2] ?? 0, rgba[i + 3] ?? 0];
    }

    // Verify that WDDM scanout presentation is not suppressed when the scanout descriptor is
    // unchanged (scanout memory can change without a ScanoutState generation bump).
    //
    // Mutate a single pixel in the scanout backing store, force a present pass via the
    // deterministic screenshot API (which calls into `handleTick()` in scanout mode), then
    // confirm the presented output updates accordingly.
    // Pick a point inside the top-left (red) quadrant, but avoid the other sampled points
    // (cursor nearby, quadrant probes) so debug samples stay consistent.
    const scanoutUpdateX = 16;
    const scanoutUpdateY = 16;
    const scanoutUpdateBefore = sample(presentedRgba8, presentedWidth, scanoutUpdateX, scanoutUpdateY);
    let scanoutUpdateAfter = scanoutUpdateBefore;
    if (pass) {
      {
        const pxOff = scanoutUpdateY * pitchBytes + scanoutUpdateX * 4;
        // BGRX => green pixel (0,255,0,255) after swizzle + alpha policy.
        backing[pxOff + 0] = 0; // B
        backing[pxOff + 1] = 255; // G
        backing[pxOff + 2] = 0; // R
        backing[pxOff + 3] = 0; // X
      }

      const expected = [0, 255, 0, 255];
      const deadline = performance.now() + 2000;
      while (performance.now() < deadline) {
        // Force a present pass in scanout mode via screenshot request (awaited in worker).
        await requestScreenshot();
        const shot = await requestPresentedScreenshot(false);
        const w = Number(shot.width) | 0;
        const h = Number(shot.height) | 0;
        if (w <= scanoutUpdateX || h <= scanoutUpdateY) {
          await sleep(20);
          continue;
        }
        const rgba = new Uint8Array(shot.rgba8);
        scanoutUpdateAfter = sample(rgba, w, scanoutUpdateX, scanoutUpdateY);
        if (
          scanoutUpdateAfter[0] === expected[0] &&
          scanoutUpdateAfter[1] === expected[1] &&
          scanoutUpdateAfter[2] === expected[2] &&
          scanoutUpdateAfter[3] === expected[3]
        ) {
          break;
        }
        await sleep(20);
      }
      if (
        scanoutUpdateAfter[0] !== expected[0] ||
        scanoutUpdateAfter[1] !== expected[1] ||
        scanoutUpdateAfter[2] !== expected[2] ||
        scanoutUpdateAfter[3] !== expected[3]
      ) {
        throw new Error(
          `Scanout update not reflected in presented output (expected ${expected.join(",")} at ${scanoutUpdateX},${scanoutUpdateY}; got ${scanoutUpdateAfter.join(",")})`,
        );
      }
    }

    log(`backend=${backend}`);
    log(`hash=${hash} expected=${expectedHash} ${pass ? "PASS" : "FAIL"}`);
    log(`sourceHash=${sourceHash} expectedSource=${expectedSourceHash}`);
    log(`shared.frame_dirty.cleared=${sharedDirtyCleared}`);

    const waitFor = async (predicate: () => boolean, timeoutMs: number, label: string): Promise<void> => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        if (predicate()) return;
        await sleep(25);
      }
      throw new Error(`Timed out waiting for ${label}`);
    };

    const waitForNewEvent = async (
      predicate: (ev: any) => boolean,
      startIndex: number,
      timeoutMs: number,
      label: string,
    ): Promise<any> => {
      await waitFor(() => gpuEvents.slice(startIndex).some(predicate), timeoutMs, label);
      for (let i = gpuEvents.length - 1; i >= startIndex; i -= 1) {
        const ev = gpuEvents[i];
        if (predicate(ev)) return ev;
      }
      return null;
    };

    const postContextLoss = (action: "lose" | "restore") => {
      worker.postMessage({ ...GPU_MESSAGE_BASE, type: "debug_context_loss", action });
    };

    const runContextLossRecovery = async () => {
      try {
        // Ensure we have at least one stats snapshot so countersBefore is meaningful.
        await waitFor(() => lastStats != null && lastStats.type === "stats", 15_000, "gpu-worker stats");
      } catch (err) {
        return { ok: false, error: formatOneLineError(err, 512) };
      }

      const countersBefore = lastStats?.counters ?? null;
      const readyCountBefore = readyCount;

      const debugBeforeIndex = gpuEvents.length;
      postContextLoss("lose");
      let loseOk: boolean | undefined = undefined;
      try {
        const ev = await waitForNewEvent(
          (e) => e && e.category === "Debug" && typeof e.message === "string" && e.message.includes("action=lose"),
          debugBeforeIndex,
          5000,
          "debug_context_loss lose",
        );
        loseOk = typeof ev?.message === "string" ? ev.message.includes("ok=true") : undefined;
      } catch (err) {
        return { ok: false, error: formatOneLineError(err, 512) };
      }
      if (loseOk !== true) {
        return { ok: false, error: "debug_context_loss lose did not succeed", loseOk };
      }

      // Ensure the worker observed context loss and entered the device-lost state before requesting restore.
      try {
        await waitForNewEvent(
          (e) => e && e.category === "DeviceLost",
          debugBeforeIndex,
          15_000,
          "WebGL context lost (DeviceLost event)",
        );
      } catch (err) {
        return { ok: false, error: formatOneLineError(err, 512), loseOk };
      }

      const debugRestoreIndex = gpuEvents.length;
      postContextLoss("restore");
      let restoreOk: boolean | undefined = undefined;
      try {
        const ev = await waitForNewEvent(
          (e) => e && e.category === "Debug" && typeof e.message === "string" && e.message.includes("action=restore"),
          debugRestoreIndex,
          5000,
          "debug_context_loss restore",
        );
        restoreOk = typeof ev?.message === "string" ? ev.message.includes("ok=true") : undefined;
      } catch (err) {
        return { ok: false, error: formatOneLineError(err, 512) };
      }
      if (restoreOk !== true) {
        return { ok: false, error: "debug_context_loss restore did not succeed", loseOk, restoreOk };
      }

      // Wait for the worker to re-emit READY after recovery.
      try {
        await waitFor(() => readyCount >= readyCountBefore + 1, 15_000, "gpu-worker recovery ready");
      } catch (err) {
        return { ok: false, error: formatOneLineError(err, 512), loseOk, restoreOk };
      }

      // Send a few ticks while the legacy shared framebuffer remains PRESENTED. This mimics the
      // runtime scheduler behavior (WDDM scanout must still drive presentation even when the
      // shared-framebuffer pacing state is idle).
      for (let i = 0; i < 3; i += 1) {
        worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
        await sleep(25);
      }

      // Wait for a new stats snapshot that reflects the recovery.
      if (countersBefore && typeof countersBefore.recoveries_succeeded === "number") {
        try {
          await waitFor(
            () =>
              lastStats != null &&
              lastStats.type === "stats" &&
              typeof lastStats.counters?.recoveries_succeeded === "number" &&
              lastStats.counters.recoveries_succeeded >= countersBefore.recoveries_succeeded + 1,
            15_000,
            "recovery counters",
          );
        } catch (err) {
          return { ok: false, error: formatOneLineError(err, 512), loseOk, restoreOk };
        }
      }

      // Capture presented output post-recovery.
      const presentedAfter = await requestPresentedScreenshot(false);
      const presentedAfterWidth = Number(presentedAfter.width) | 0;
      const presentedAfterHeight = Number(presentedAfter.height) | 0;
      const presentedAfterRgba8 = new Uint8Array(presentedAfter.rgba8);
      const afterHash = fnv1a32Hex(presentedAfterRgba8);

      const samplesAfter =
        presentedAfterWidth === presentedWidth && presentedAfterHeight === presentedHeight
          ? {
              topLeft: sample(presentedAfterRgba8, presentedAfterWidth, 8, 8),
              topRight: sample(presentedAfterRgba8, presentedAfterWidth, presentedAfterWidth - 9, 8),
              bottomLeft: sample(presentedAfterRgba8, presentedAfterWidth, 8, presentedAfterHeight - 9),
              bottomRight: sample(presentedAfterRgba8, presentedAfterWidth, presentedAfterWidth - 9, presentedAfterHeight - 9),
            }
          : {
              topLeft: sample(presentedAfterRgba8, presentedAfterWidth, 0, 0),
              topRight: sample(presentedAfterRgba8, presentedAfterWidth, Math.max(0, presentedAfterWidth - 1), 0),
              bottomLeft: sample(presentedAfterRgba8, presentedAfterWidth, 0, Math.max(0, presentedAfterHeight - 1)),
              bottomRight: sample(
                presentedAfterRgba8,
                presentedAfterWidth,
                Math.max(0, presentedAfterWidth - 1),
                Math.max(0, presentedAfterHeight - 1),
              ),
            };

      return {
        ok: afterHash === expectedHash,
        ...(afterHash === expectedHash ? {} : { error: `presented hash mismatch after recovery (got=${afterHash} expected=${expectedHash})` }),
        loseOk,
        restoreOk,
        before: { readyCount: readyCountBefore, counters: countersBefore },
        after: {
          readyCount,
          counters: lastStats?.counters ?? null,
          hash: afterHash,
          expectedHash,
          samples: samplesAfter,
        },
      };
    };

    window.__aeroTest = {
      ready: true,
      backend,
      ...(sharedDirtyCleared
        ? {}
        : { error: `shared framebuffer frame_dirty was not cleared under WDDM scanout (got ${sharedDirtyAfter})` }),
      hash,
      expectedHash,
      sourceHash,
      expectedSourceHash,
      pass,
      sharedDirtyCleared,
      metrics: lastMetrics,
      stats: lastStats,
      events: gpuEvents,
      samplePixels: async () => ({
        backend,
        cursor: { x: cursorX, y: cursorY, pixel: cursorPixel, nearby: cursorNearby },
        source: {
          width: sourceWidth,
          height: sourceHeight,
          topLeft: sample(sourceRgba8, sourceWidth, 8, 8),
          topRight: sample(sourceRgba8, sourceWidth, sourceWidth - 9, 8),
          bottomLeft: sample(sourceRgba8, sourceWidth, 8, sourceHeight - 9),
          bottomRight: sample(sourceRgba8, sourceWidth, sourceWidth - 9, sourceHeight - 9),
        },
        presented: {
          width: presentedWidth,
          height: presentedHeight,
          topLeft: sample(presentedRgba8, presentedWidth, 8, 8),
          topRight: sample(presentedRgba8, presentedWidth, presentedWidth - 9, 8),
          bottomLeft: sample(presentedRgba8, presentedWidth, 8, presentedHeight - 9),
          bottomRight: sample(presentedRgba8, presentedWidth, presentedWidth - 9, presentedHeight - 9),
        },
        scanoutUpdate: {
          x: scanoutUpdateX,
          y: scanoutUpdateY,
          before: scanoutUpdateBefore,
          after: scanoutUpdateAfter,
        },
      }),
      cursorOk,
      runContextLossRecovery,
    };
  } catch (err) {
    renderError(formatOneLineError(err, 512));
  }
}

void main();
