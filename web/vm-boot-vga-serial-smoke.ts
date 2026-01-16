import { startFrameScheduler } from "./src/main/frameScheduler";
import { FRAME_DIRTY, FRAME_STATUS_INDEX, GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION, isGpuWorkerMessageBase } from "./src/ipc/gpu-protocol";
import { SHARED_FRAMEBUFFER_HEADER_U32_LEN, SharedFramebufferHeaderIndex } from "./src/ipc/shared-layout";
import { probeRemoteDisk } from "./src/platform/remote_disk";
import { WorkerCoordinator } from "./src/runtime/coordinator";
import type { AeroConfig } from "./src/config/aero_config";
import type { DiskImageMetadata } from "./src/storage/metadata";
import { formatOneLineError } from "./src/text";

const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      pass?: boolean;
      error?: string;
      serial?: string;
      samples?: { vgaPixels: number[][] };
      metrics?: { framesReceived: number; framesPresented: number; framesDropped: number };
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function renderError(message: string) {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function samplePixel(rgba: Uint8Array, width: number, x: number, y: number): number[] {
  const i = (y * width + x) * 4;
  return [rgba[i + 0] ?? 0, rgba[i + 1] ?? 0, rgba[i + 2] ?? 0, rgba[i + 3] ?? 0];
}

async function main() {
  const canvas = $("frame");
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError("Canvas element not found");
    return;
  }

  const status = $("status");
  const log = (line: string) => {
    if (status) status.textContent += `${line}\n`;
  };

  if (!("transferControlToOffscreen" in canvas)) {
    renderError("OffscreenCanvas is not supported in this browser.");
    return;
  }

  const coordinator = new WorkerCoordinator();
  const support = coordinator.checkSupport();
  if (!support.ok) {
    renderError(support.reason ?? "Shared memory unsupported");
    return;
  }

  try {
    const config = {
      // This harness only needs enough guest RAM for:
      // - the boot sector at 0x7C00, and
      // - the VGA text region at 0xB8000 (used to publish the deterministic VGA signature).
      //
      // Keep guest RAM tiny to reduce Playwright/CI shared `WebAssembly.Memory` pressure.
      guestMemoryMiB: 1,
      // This harness boots a VGA/serial-only guest; it does not require a BAR1/VRAM aperture.
      vramMiB: 0,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      // `activeDiskImage` is deprecated as a "VM mode" toggle. The harness drives VM boot via
      // `coordinator.setBootDisks(...)` instead.
      activeDiskImage: null,
      logLevel: "info",
    } satisfies AeroConfig;

    coordinator.start(config);

    const cpuWorker = coordinator.getCpuWorker();
    const ioWorker = coordinator.getWorker("io");
    const gpuWorker = coordinator.getWorker("gpu");
    const frameStateSab = coordinator.getFrameStateSab();
    const sharedFramebuffer = coordinator.getSharedFramebuffer();

    if (!cpuWorker || !ioWorker || !gpuWorker || !frameStateSab || !sharedFramebuffer) {
      throw new Error("Runtime workers did not expose expected shared resources.");
    }

    const header = new Int32Array(sharedFramebuffer.sab, sharedFramebuffer.offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
    const width = Atomics.load(header, SharedFramebufferHeaderIndex.WIDTH);
    const height = Atomics.load(header, SharedFramebufferHeaderIndex.HEIGHT);

    const dpr = 1;
    canvas.width = Math.max(1, Math.round(width * dpr));
    canvas.height = Math.max(1, Math.round(height * dpr));
    canvas.style.width = `${Math.min(width, 320)}px`;
    canvas.style.height = `${Math.min(height, 240)}px`;

    const offscreen = canvas.transferControlToOffscreen();

    const pendingScreenshots = new Map<
      number,
      {
        resolve: (msg: { width: number; height: number; pixels: ArrayBuffer }) => void;
        reject: (err: unknown) => void;
      }
    >();
    let nextRequestId = 1;

    let presenterReadyResolved = false;
    let presenterReadyResolve: (() => void) | null = null;
    let presenterReadyReject: ((err: unknown) => void) | null = null;
    const presenterReady = new Promise<void>((resolve, reject) => {
      presenterReadyResolve = resolve;
      presenterReadyReject = reject;
    });

    gpuWorker.addEventListener("message", (event: MessageEvent) => {
      const msg = event.data as unknown;
      if (!isGpuWorkerMessageBase(msg) || typeof (msg as { type?: unknown }).type !== "string") return;
      const typed = msg as { type: string; requestId?: number; width?: number; height?: number; rgba8?: ArrayBuffer; message?: string };
      if (typed.type === "ready") {
        presenterReadyResolved = true;
        presenterReadyResolve?.();
        presenterReadyResolve = null;
        presenterReadyReject = null;
        return;
      }
      if (typed.type === "screenshot") {
        const pending = pendingScreenshots.get(typed.requestId ?? -1);
        if (!pending) return;
        pendingScreenshots.delete(typed.requestId ?? -1);
        pending.resolve({ width: typed.width ?? 0, height: typed.height ?? 0, pixels: typed.rgba8 ?? new ArrayBuffer(0) });
        return;
      }
      if (typed.type === "error") {
        log(`gpu-worker error: ${formatOneLineError(typed.message, 512, "unknown")}`);
        if (!presenterReadyResolved && presenterReadyReject) {
          presenterReadyReject(new Error(formatOneLineError(typed.message, 512, "gpu-worker init error")));
          presenterReadyResolve = null;
          presenterReadyReject = null;
        }
      }
    });

    const frameScheduler = startFrameScheduler({
      gpuWorker,
      sharedFrameState: frameStateSab,
      sharedFramebuffer: sharedFramebuffer.sab,
      sharedFramebufferOffsetBytes: sharedFramebuffer.offsetBytes,
      canvas: offscreen,
      initOptions: {
        forceBackend: "webgl2_raw",
        disableWebGpu: true,
        outputWidth: width,
        outputHeight: height,
        dpr,
      },
      showDebugOverlay: false,
    });

    await presenterReady;

    const requestScreenshot = (): Promise<{ width: number; height: number; pixels: ArrayBuffer }> => {
      const requestId = nextRequestId++;
      gpuWorker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId });
      return new Promise((resolve, reject) => {
        pendingScreenshots.set(requestId, { resolve, reject });
        setTimeout(() => {
          const pending = pendingScreenshots.get(requestId);
          if (!pending) return;
          pendingScreenshots.delete(requestId);
          reject(new Error("screenshot request timed out"));
        }, 5000);
      });
    };

    // Open the deterministic boot fixture as a remote-range disk.
    //
    // Note: `RemoteStreamingDisk` requires HTTP Range (206) support. The repo's
    // Vite dev server does not guarantee correct range semantics for arbitrary
    // assets, so this harness requires an explicit `?diskUrl=` parameter (the
    // Playwright spec starts a tiny range-capable server).
    const search = new URLSearchParams(window.location.search);
    const diskUrl = search.get("diskUrl");
    if (!diskUrl) {
      throw new Error("Missing ?diskUrl=... (must point to a range-capable disk image server).");
    }
    const probe = await probeRemoteDisk(diskUrl);
    const diskSize = probe.size;
    const diskId = typeof crypto?.randomUUID === "function" ? crypto.randomUUID() : `boot_${Date.now()}`;

    const hdd = {
      source: "remote",
      id: diskId,
      name: "boot_vga_serial.img",
      kind: "hdd",
      format: "raw",
      sizeBytes: diskSize,
      createdAtMs: Date.now(),
      remote: {
        imageId: "boot_vga_serial",
        version: "1",
        delivery: "range",
        urls: { url: diskUrl },
        ...(probe.etag
          ? { validator: { etag: probe.etag } }
          : probe.lastModified
            ? { validator: { lastModified: probe.lastModified } }
            : {}),
      },
      cache: {
        // Use IndexedDB-backed cache/overlay so the harness works even when OPFS is unavailable.
        backend: "idb",
        chunkSizeBytes: 1024 * 1024,
        fileName: `${diskId}.cache`,
        overlayFileName: `${diskId}.overlay`,
        overlayBlockSizeBytes: 1024 * 1024,
      },
    } satisfies DiskImageMetadata;
    coordinator.setBootDisks({ hddId: diskId }, hdd, null);
    log(`disk: ${diskUrl} (${diskSize} bytes)`);

    // Wait for the CPU worker to boot the sector and emit the serial signature.
    const serialDeadline = performance.now() + 10_000;
    while (performance.now() < serialDeadline) {
      const out = coordinator.getSerialOutputText();
      if (out.includes("AERO!")) break;
      await sleep(10);
    }
    const serial = coordinator.getSerialOutputText();
    log(`serial: ${JSON.stringify(serial)}`);
    if (!serial.includes("AERO!")) {
      throw new Error("Timed out waiting for serial signature (expected 'AERO!').");
    }

    // Ensure at least one frame has been presented (GPU worker path is active).
    const presentedDeadline = performance.now() + 5000;
    while (performance.now() < presentedDeadline) {
      if (frameScheduler.getMetrics().framesPresented > 0) break;
      // Kick the scheduler if a frame is pending.
      const st = Atomics.load(new Int32Array(frameStateSab), FRAME_STATUS_INDEX);
      if (st === FRAME_DIRTY) {
        // The scheduler already polls, but a tiny nudge helps in headless runs.
      }
      await sleep(10);
    }

    // Screenshot loop: wait until we observe the VGA signature pixels.
    const expected = [
      [65, 31, 0, 255], // A
      [69, 31, 0, 255], // E
      [82, 31, 0, 255], // R
      [79, 31, 0, 255], // O
      [33, 31, 0, 255], // !
    ];

    let vgaPixels: number[][] = [];
    let pass = false;
    const vgaDeadline = performance.now() + 5000;
    while (performance.now() < vgaDeadline) {
      const shot = await requestScreenshot();
      const rgba = new Uint8Array(shot.pixels);
      vgaPixels = expected.map((_, i) => samplePixel(rgba, shot.width, i, 0));
      if (vgaPixels.every((p, i) => p.join(",") === expected[i]!.join(","))) {
        pass = true;
        break;
      }
      await sleep(50);
    }

    log(`vgaPixels: ${JSON.stringify(vgaPixels)}`);
    log(pass ? "PASS" : "FAIL");

    const metrics = frameScheduler.getMetrics();
    frameScheduler.stop();
    coordinator.stop();

    window.__aeroTest = {
      ready: true,
      pass,
      serial,
      samples: { vgaPixels },
      metrics,
    };
  } catch (err) {
    coordinator.stop();
    renderError(formatOneLineError(err, 512));
  }
}

void main();
