import { expect, test } from "@playwright/test";

test("runtime workers: gpu worker presents ScanoutState framebuffer (B8G8R8X8 -> RGBA)", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "OffscreenCanvas + WebGL2-in-worker coverage is Chromium-only for now.");

  await page.goto("/web/blank.html");

  const result = await page.evaluate(async () => {
    const { WorkerCoordinator } = await import("/web/src/runtime/coordinator.ts");
    const { startFrameScheduler } = await import("/web/src/main/frameScheduler.ts");
    const gpuProto = await import("/web/src/ipc/gpu-protocol.ts");
    const sharedLayout = await import("/web/src/runtime/shared_layout.ts");
    const scanout = await import("/web/src/ipc/scanout_state.ts");
    const sharedFb = await import("/web/src/ipc/shared-layout.ts");
    const { formatOneLineUtf8 } = await import("/web/src/text.ts");

    const MAX_ERROR_BYTES = 512;

    function formatOneLineError(err: unknown): string {
      const msg = err instanceof Error ? err.message : err;
      return formatOneLineUtf8(String(msg ?? ""), MAX_ERROR_BYTES) || "Error";
    }

    const coordinator = new WorkerCoordinator();
    const support = coordinator.checkSupport();
    if (!support.ok) {
      return { ok: false, error: support.reason ?? "Shared memory unsupported" };
    }

    const config = {
      // Keep guest RAM tiny: this test only needs enough space for a small scanout test pattern.
      // 1MiB also keeps the legacy shared framebuffer out of guest RAM (disabling the moving WASM
      // demo and keeping the CPU worker on the deterministic JS fallback path).
      guestMemoryMiB: 1,
      vramMiB: 0,
      enableWorkers: true,
      enableWebGPU: false,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
    } as const;

    coordinator.start(config as any);
    coordinator.setBootDisks({}, null, null);

    const gpuWorker = coordinator.getWorker("gpu");
    const frameStateSab = coordinator.getFrameStateSab();
    const sharedFramebuffer = coordinator.getSharedFramebuffer();
    const scanoutState = coordinator.getScanoutState();
    const guestMemory = coordinator.getGuestMemory();
    const status = coordinator.getStatusView();

    if (!gpuWorker || !frameStateSab || !sharedFramebuffer || !scanoutState || !guestMemory || !status) {
      coordinator.stop();
      return { ok: false, error: "Missing runtime worker/shared memory handles" };
    }

    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);
    const offscreen = canvas.transferControlToOffscreen();

    const GPU_MESSAGE_BASE = { protocol: gpuProto.GPU_PROTOCOL_NAME, protocolVersion: gpuProto.GPU_PROTOCOL_VERSION } as const;

    const pendingScreenshots = new Map<number, { resolve: (msg: any) => void; reject: (err: unknown) => void }>();
    let nextRequestId = 1;

    let readyResolved = false;
    let readyResolve: (() => void) | null = null;
    let readyReject: ((err: unknown) => void) | null = null;
    const ready = new Promise<void>((resolve, reject) => {
      readyResolve = resolve;
      readyReject = reject;
    });

    const onWorkerMessage = (event: MessageEvent) => {
      const msg = event.data as any;
      if (!gpuProto.isGpuWorkerMessageBase(msg) || typeof msg.type !== "string") return;

      if (msg.type === "ready") {
        readyResolved = true;
        readyResolve?.();
        readyResolve = null;
        readyReject = null;
        return;
      }
      if (msg.type === "screenshot") {
        const pending = pendingScreenshots.get(msg.requestId);
        if (!pending) return;
        pendingScreenshots.delete(msg.requestId);
        pending.resolve(msg);
        return;
      }
      if (msg.type === "error") {
        if (!readyResolved && readyReject) {
          readyReject(new Error(String(msg.message ?? "gpu worker init error")));
          readyResolve = null;
          readyReject = null;
        }
      }
    };

    gpuWorker.addEventListener("message", onWorkerMessage);

    const scheduler = startFrameScheduler({
      gpuWorker,
      sharedFrameState: frameStateSab,
      sharedFramebuffer: sharedFramebuffer.sab,
      sharedFramebufferOffsetBytes: sharedFramebuffer.offsetBytes,
      scanoutState: scanoutState.sab,
      scanoutStateOffsetBytes: scanoutState.offsetBytes,
      canvas: offscreen,
      initOptions: {
        forceBackend: "webgl2_raw",
        disableWebGpu: true,
        dpr: 1,
      },
      showDebugOverlay: false,
    });

    const requestScreenshot = (): Promise<any> => {
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

    const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

    try {
      await ready;

      const header = new Int32Array(sharedFramebuffer.sab, sharedFramebuffer.offsetBytes, sharedFb.SHARED_FRAMEBUFFER_HEADER_U32_LEN);
      const legacyWidth = Atomics.load(header, sharedFb.SharedFramebufferHeaderIndex.WIDTH) >>> 0;
      const legacyHeight = Atomics.load(header, sharedFb.SharedFramebufferHeaderIndex.HEIGHT) >>> 0;

      // Wait for the CPU demo to publish at least one frame.
      while ((Atomics.load(header, sharedFb.SharedFramebufferHeaderIndex.FRAME_SEQ) >>> 0) < 2) {
        await sleep(5);
      }

      const legacyShot = await requestScreenshot();
      const legacyOk = legacyShot.width === legacyWidth && legacyShot.height === legacyHeight;

      // Publish a small B8G8R8X8 scanout backed by guest RAM.
      const scanoutWidth = 3;
      const scanoutHeight = 2;
      const pitchBytes = 16; // 12 bytes of pixels + 4 bytes padding per row.
      // Keep this above the VGA text region (0xB8000) and other low-memory demo scratch buffers.
      // 0x0C0_000 = 768KiB.
      const basePaddr = 0x0c0_000;

      const guestLayout = sharedLayout.readGuestRamLayoutFromStatus(status);
      const guestU8 = new Uint8Array(guestMemory.buffer as ArrayBuffer, guestLayout.guest_base, guestLayout.guest_size);
      const ramOffset = sharedLayout.guestPaddrToRamOffset(guestLayout, basePaddr);
      if (ramOffset == null) {
        throw new Error("guestPaddrToRamOffset returned null for test pattern");
      }

      const src = new Uint8Array(pitchBytes * scanoutHeight);
      // Row 0 pixels (B,G,R,X).
      src.set([0x10, 0x20, 0x30, 0x00], 0);
      src.set([0x40, 0x50, 0x60, 0x00], 4);
      src.set([0x70, 0x80, 0x90, 0x00], 8);
      src.set([0xaa, 0xbb, 0xcc, 0xdd], 12); // padding
      // Row 1 pixels.
      src.set([0x01, 0x02, 0x03, 0x00], 16);
      src.set([0x04, 0x05, 0x06, 0x00], 20);
      src.set([0x07, 0x08, 0x09, 0x00], 24);
      src.set([0xee, 0xff, 0x11, 0x22], 28); // padding

      guestU8.set(src, ramOffset);

      const words = scanout.wrapScanoutState(scanoutState.sab, scanoutState.offsetBytes);
      scanout.publishScanoutState(words, {
        source: scanout.SCANOUT_SOURCE_LEGACY_VBE_LFB,
        basePaddrLo: basePaddr >>> 0,
        basePaddrHi: 0,
        width: scanoutWidth,
        height: scanoutHeight,
        pitchBytes,
        format: scanout.SCANOUT_FORMAT_B8G8R8X8,
      });

      const expected = new Uint8Array(scanoutWidth * scanoutHeight * 4);
      // Row 0 expected RGBA.
      expected.set([0x30, 0x20, 0x10, 0xff], 0);
      expected.set([0x60, 0x50, 0x40, 0xff], 4);
      expected.set([0x90, 0x80, 0x70, 0xff], 8);
      // Row 1 expected RGBA.
      expected.set([0x03, 0x02, 0x01, 0xff], 12);
      expected.set([0x06, 0x05, 0x04, 0xff], 16);
      expected.set([0x09, 0x08, 0x07, 0xff], 20);

      const deadline = performance.now() + 5000;
      let scanoutOk = false;
      let lastMismatch: number[] | null = null;
      while (performance.now() < deadline) {
        const shot = await requestScreenshot();
        if (shot.width !== scanoutWidth || shot.height !== scanoutHeight) {
          await sleep(25);
          continue;
        }
        const got = new Uint8Array(shot.rgba8);
        if (got.length !== expected.length) {
          await sleep(25);
          continue;
        }
        let match = true;
        for (let i = 0; i < expected.length; i += 1) {
          if (got[i] !== expected[i]) {
            match = false;
            lastMismatch = [i, got[i] ?? -1, expected[i] ?? -1];
            break;
          }
        }
        if (match) {
          scanoutOk = true;
          break;
        }
        await sleep(25);
      }

      let wddmOk = false;
      let lastMismatchWddm: number[] | null = null;
      if (scanoutOk) {
        // Mutate the guest pattern and switch to WDDM source to ensure the worker
        // still reads from the ScanoutState descriptor in WDDM mode.
        src.set([0x99, 0x88, 0x77, 0x00], 0);
        src.set([0x66, 0x55, 0x44, 0x00], 4);
        src.set([0x33, 0x22, 0x11, 0x00], 8);
        src.set([0xaa, 0xbb, 0xcc, 0xdd], 12);
        src.set([0x09, 0x0a, 0x0b, 0x00], 16);
        src.set([0x0c, 0x0d, 0x0e, 0x00], 20);
        src.set([0x0f, 0x10, 0x11, 0x00], 24);
        src.set([0xee, 0xff, 0x11, 0x22], 28);
        guestU8.set(src, ramOffset);

        scanout.publishScanoutState(words, {
          source: scanout.SCANOUT_SOURCE_WDDM,
          basePaddrLo: basePaddr >>> 0,
          basePaddrHi: 0,
          width: scanoutWidth,
          height: scanoutHeight,
          pitchBytes,
          format: scanout.SCANOUT_FORMAT_B8G8R8X8,
        });

        const expectedWddm = new Uint8Array(scanoutWidth * scanoutHeight * 4);
        expectedWddm.set([0x77, 0x88, 0x99, 0xff], 0);
        expectedWddm.set([0x44, 0x55, 0x66, 0xff], 4);
        expectedWddm.set([0x11, 0x22, 0x33, 0xff], 8);
        expectedWddm.set([0x0b, 0x0a, 0x09, 0xff], 12);
        expectedWddm.set([0x0e, 0x0d, 0x0c, 0xff], 16);
        expectedWddm.set([0x11, 0x10, 0x0f, 0xff], 20);

        const deadlineWddm = performance.now() + 5000;
        while (performance.now() < deadlineWddm) {
          const shot = await requestScreenshot();
          if (shot.width !== scanoutWidth || shot.height !== scanoutHeight) {
            await sleep(25);
            continue;
          }
          const got = new Uint8Array(shot.rgba8);
          if (got.length !== expectedWddm.length) {
            await sleep(25);
            continue;
          }
          let match = true;
          for (let i = 0; i < expectedWddm.length; i += 1) {
            if (got[i] !== expectedWddm[i]) {
              match = false;
              lastMismatchWddm = [i, got[i] ?? -1, expectedWddm[i] ?? -1];
              break;
            }
          }
          if (match) {
            wddmOk = true;
            break;
          }
          await sleep(25);
        }
      }

      return { ok: true, legacyOk, scanoutOk, wddmOk, legacyWidth, legacyHeight, lastMismatch, lastMismatchWddm };
    } catch (err) {
      return { ok: false, error: formatOneLineError(err) };
    } finally {
      scheduler.stop();
      coordinator.stop();
      gpuWorker.removeEventListener("message", onWorkerMessage);
    }
  });

  expect(result.ok).toBe(true);
  if (!result.ok) {
    throw new Error(String(result.error ?? "runtime scanout test failed"));
  }
  expect(result.legacyOk).toBe(true);
  expect(result.scanoutOk).toBe(true);
  expect(result.wddmOk).toBe(true);
});
