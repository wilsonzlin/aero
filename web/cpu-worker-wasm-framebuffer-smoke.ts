import { fnv1a32Hex } from "./src/utils/fnv1a";
import { WorkerCoordinator } from "./src/runtime/coordinator";
import {
  CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
  CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES,
  CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
  CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
  computeGuestRamLayout,
} from "./src/runtime/shared_layout";
import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
} from "./src/ipc/shared-layout";
import { formatOneLineError } from "./src/text";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      pass?: boolean;
      error?: string;
      hashes?: { first: string; second: string };
      frames?: { firstSeq: number; secondSeq: number };
      samples?: {
        firstPixel00: number[];
        secondPixel00: number[];
        firstPixelAway: number[];
        secondPixelAway: number[];
      };
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function renderError(message: string) {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message };
}

function samplePixel(buf: Uint8Array, strideBytes: number, x: number, y: number): number[] {
  const i = y * strideBytes + x * 4;
  return [buf[i + 0] ?? 0, buf[i + 1] ?? 0, buf[i + 2] ?? 0, buf[i + 3] ?? 0];
}

async function main() {
  const status = $("status");
  const log = (line: string) => {
    if (status) status.textContent += `${line}\n`;
  };

  const coordinator = new WorkerCoordinator();

  try {
    const requiredLayout = computeSharedFramebufferLayout(
      CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH,
      CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT,
      CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH * 4,
      FramebufferFormat.RGBA8,
      CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE,
    );
    const requiredBytes = CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES + requiredLayout.totalBytes;
    // Keep a small floor so the CPU worker's other demo paths (VGA staging buffer,
    // disk demo scratch, etc.) have room even though this test only asserts the
    // shared framebuffer written by WASM.
    // 8MiB is ample for the demo framebuffer region (~4.5MiB incl. offset) + a bit of scratch, and
    // keeps Playwright/CI shared-memory pressure down.
    const guestMemoryMiB = Math.max(8, Math.ceil((requiredBytes + 1024 * 1024) / (1024 * 1024)));
    const guestLayout = computeGuestRamLayout(guestMemoryMiB * 1024 * 1024);
    const demoLinearBaseOffsetBytes = guestLayout.guest_base + CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES;

    coordinator.start({
      guestMemoryMiB,
      // This harness only validates the CPU worker's shared framebuffer demo; no VRAM aperture needed.
      vramMiB: 0,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    });
    coordinator.setBootDisks({}, null, null);

    const guestMemory = coordinator.getGuestMemory();
    if (!guestMemory) {
      throw new Error("Guest memory was not initialized.");
    }

    const guestSab = guestMemory.buffer as unknown as SharedArrayBuffer;
    const header = new Int32Array(guestSab, demoLinearBaseOffsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);

    // Allow time for the threaded WASM module to initialize in the CPU worker
    // (and for it to start publishing frames). In CI this can take longer than a
    // few frames, even with a precompiled WebAssembly.Module.
    const deadlineMs = performance.now() + 10_000;
    while (performance.now() < deadlineMs) {
      const magic = Atomics.load(header, SharedFramebufferHeaderIndex.MAGIC);
      const version = Atomics.load(header, SharedFramebufferHeaderIndex.VERSION);
      if (magic === SHARED_FRAMEBUFFER_MAGIC && version === SHARED_FRAMEBUFFER_VERSION) break;
      await sleep(1);
    }

    const magic = Atomics.load(header, SharedFramebufferHeaderIndex.MAGIC);
    const version = Atomics.load(header, SharedFramebufferHeaderIndex.VERSION);
    if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) {
      throw new Error(
        `Shared framebuffer header not initialized (magic=0x${magic.toString(16)} version=${version}).`,
      );
    }

    const layout = layoutFromHeader(header);
    log(`layout: ${layout.width}x${layout.height} stride=${layout.strideBytes} tileSize=${layout.tileSize}`);

    if (layout.width !== CPU_WORKER_DEMO_FRAMEBUFFER_WIDTH || layout.height !== CPU_WORKER_DEMO_FRAMEBUFFER_HEIGHT) {
      throw new Error(`Unexpected demo framebuffer size: got ${layout.width}x${layout.height}`);
    }
    if (layout.tileSize !== CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE) {
      throw new Error(`Unexpected demo framebuffer tileSize: got ${layout.tileSize}`);
    }

    const waitForSeqAtLeast = async (seq: number) => {
      while (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) < seq) {
        if (performance.now() > deadlineMs) throw new Error("Timed out waiting for CPU worker frames.");
        await sleep(1);
      }
    };

    const capture = () => {
      for (let attempt = 0; attempt < 10; attempt += 1) {
        const seq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) >>> 0;
        const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) >>> 0;
        const bufSeq = Atomics.load(
          header,
          active === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
        );
        if (bufSeq !== seq) continue;

        const offset = demoLinearBaseOffsetBytes + layout.framebufferOffsets[active];
        const slot = new Uint8Array(guestSab, offset, layout.strideBytes * layout.height);

        // Hash a small prefix (enough to catch changes without scanning the whole frame).
        const prefix = slot.subarray(0, Math.min(1024, slot.byteLength));
        const hash = fnv1a32Hex(prefix);
        const pixel00 = samplePixel(slot, layout.strideBytes, 0, 0);
        const pixelAway = samplePixel(
          slot,
          layout.strideBytes,
          Math.min(layout.width - 1, CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE + 1),
          Math.min(layout.height - 1, CPU_WORKER_DEMO_FRAMEBUFFER_TILE_SIZE + 1),
        );
        return { seq, hash, pixel00, pixelAway };
      }
      throw new Error("Failed to capture a consistent published frame.");
    };

    await waitForSeqAtLeast(1);
    const first = capture();
    let second = first;

    // Wait until we observe an update outside the top-left tile. This
    // distinguishes the WASM demo's moving gradient from the JS fallback which
    // only toggles the top-left tile.
    while (performance.now() < deadlineMs) {
      await waitForSeqAtLeast(second.seq + 2);
      second = capture();
      if (first.hash !== second.hash && first.pixelAway.join(",") !== second.pixelAway.join(",")) {
        break;
      }
      await sleep(5);
    }

    const pass = first.hash !== second.hash && first.pixelAway.join(",") !== second.pixelAway.join(",");
    log(`first: seq=${first.seq} hash=${first.hash} pixel00=${first.pixel00.join(",")} pixelAway=${first.pixelAway.join(",")}`);
    log(`second: seq=${second.seq} hash=${second.hash} pixel00=${second.pixel00.join(",")} pixelAway=${second.pixelAway.join(",")}`);
    log(pass ? "PASS" : "FAIL (did not observe pixels changing outside top-left tile; wasm demo may be unavailable)");

    coordinator.stop();

    window.__aeroTest = {
      ready: true,
      pass,
      hashes: { first: first.hash, second: second.hash },
      frames: { firstSeq: first.seq, secondSeq: second.seq },
      samples: {
        firstPixel00: first.pixel00,
        secondPixel00: second.pixel00,
        firstPixelAway: first.pixelAway,
        secondPixelAway: second.pixelAway,
      },
    };
  } catch (err) {
    coordinator.stop();
    renderError(formatOneLineError(err, 512));
  }
}

void main();
