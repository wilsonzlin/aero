import { expect, test } from "@playwright/test";

import {
  CAPACITY_SAMPLES_INDEX as MIC_CAPACITY_SAMPLES_INDEX,
  DROPPED_SAMPLES_INDEX as MIC_DROPPED_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
  READ_POS_INDEX as MIC_READ_POS_INDEX,
  WRITE_POS_INDEX as MIC_WRITE_POS_INDEX,
} from "../../web/src/audio/mic_ring.js";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("Worker snapshot resume discards buffered mic samples (stale latency avoidance)", async ({ page }) => {
  test.setTimeout(90_000);
  test.skip(test.info().project.name !== "chromium", "Mic snapshot resume test only runs on Chromium.");
  page.setDefaultTimeout(90_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return Boolean((globalThis as any).__aeroWorkerCoordinator);
  });

  await page.evaluate(
    ({
      MIC_CAPACITY_SAMPLES_INDEX,
      MIC_DROPPED_SAMPLES_INDEX,
      MIC_HEADER_BYTES,
      MIC_HEADER_U32_LEN,
      MIC_READ_POS_INDEX,
      MIC_WRITE_POS_INDEX,
    }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const coord = (globalThis as any).__aeroWorkerCoordinator as any;

      const workerConfig = {
        guestMemoryMiB: 64,
        enableWorkers: true,
        enableWebGPU: false,
        proxyUrl: null,
        activeDiskImage: null,
        logLevel: "info",
      };
      coord.start(workerConfig);
      // io.worker waits for the first `setBootDisks` message before reporting READY.
      coord.getIoWorker()?.postMessage({ type: "setBootDisks", mounts: {}, hdd: null, cd: null });

      const capacitySamples = 4096;
      const sab = new SharedArrayBuffer(MIC_HEADER_BYTES + capacitySamples * Float32Array.BYTES_PER_ELEMENT);
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);

      Atomics.store(header, MIC_WRITE_POS_INDEX, 0);
      Atomics.store(header, MIC_READ_POS_INDEX, 0);
      Atomics.store(header, MIC_DROPPED_SAMPLES_INDEX, 0);
      Atomics.store(header, MIC_CAPACITY_SAMPLES_INDEX, capacitySamples >>> 0);

      // Expose for subsequent eval steps.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroTestMicRingSab = sab;
    },
    {
      MIC_CAPACITY_SAMPLES_INDEX,
      MIC_DROPPED_SAMPLES_INDEX,
      MIC_HEADER_BYTES,
      MIC_HEADER_U32_LEN,
      MIC_READ_POS_INDEX,
      MIC_WRITE_POS_INDEX,
    },
  );

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;
    const statuses = coord?.getWorkerStatuses?.();
    // Wait for CPU+IO to reach READY before attaching: the coordinator re-applies ring
    // attachments when audio workers report READY, and we want our direct test attachment
    // (bypassing the coordinator policy) to happen after that.
    return statuses?.cpu?.state === "ready" && statuses?.io?.state === "ready";
  });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator as any;
    // `io` is required for the IO-worker mic discard path (re-attaching calls into WASM).
    return Boolean(coord?.getWorkerWasmStatus?.("io"));
  });

  const result = await page.evaluate(
    async ({ MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const coord = (globalThis as any).__aeroWorkerCoordinator as any;
      const cpu = coord.getCpuWorker?.();
      const io = coord.getIoWorker?.();
      if (!cpu) throw new Error("Missing CPU worker");
      if (!io) throw new Error("Missing IO worker");

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const sab = (globalThis as any).__aeroTestMicRingSab as SharedArrayBuffer | undefined;
      if (!sab) throw new Error("Missing test mic SharedArrayBuffer");
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);

      const waitFor = (worker: Worker, kind: string, requestId: number) =>
        new Promise<void>((resolve, reject) => {
          const timeoutMs = 10_000;
          const timer = setTimeout(() => {
            worker.removeEventListener("message", onMessage as any);
            reject(new Error(`Timed out waiting for ${kind} (${timeoutMs}ms)`));
          }, timeoutMs);

          const onMessage = (ev: MessageEvent<any>) => {
            const msg = ev.data;
            if (!msg || typeof msg !== "object") return;
            if (msg.kind !== kind) return;
            if (msg.requestId !== requestId) return;
            clearTimeout(timer);
            worker.removeEventListener("message", onMessage as any);
            if (msg.ok !== true) {
              reject(new Error(typeof msg.error?.message === "string" ? msg.error.message : `${kind} failed`));
              return;
            }
            resolve();
          };

          worker.addEventListener("message", onMessage as any);
        });

      let nextId = 1;

      const attachMicTo = (consumer: "cpu" | "io") => {
        const attach = { type: "setMicrophoneRingBuffer", ringBuffer: sab, sampleRate: 48_000 };
        const detach = { type: "setMicrophoneRingBuffer", ringBuffer: null, sampleRate: 0 };
        if (consumer === "cpu") {
          cpu.postMessage(attach);
          io.postMessage(detach);
        } else {
          io.postMessage(attach);
          cpu.postMessage(detach);
        }
      };

      const runCycle = async (consumer: "cpu" | "io") => {
        attachMicTo(consumer);

        // Simulate the host mic producer writing samples while the VM is paused by advancing
        // `write_pos` without advancing `read_pos`.
        Atomics.store(header, MIC_READ_POS_INDEX, 0);
        Atomics.store(header, MIC_WRITE_POS_INDEX, 1000);

        const readBeforePause = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
        const writeBeforePause = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;

        // Mirror the coordinator snapshot ordering (CPU → IO) so there are no concurrent guest-side
        // port/MMIO operations while the IO worker is paused.
        const pauseCpuId = nextId++;
        cpu.postMessage({ kind: "vm.snapshot.pause", requestId: pauseCpuId });
        await waitFor(cpu, "vm.snapshot.paused", pauseCpuId);

        const pauseIoId = nextId++;
        io.postMessage({ kind: "vm.snapshot.pause", requestId: pauseIoId });
        await waitFor(io, "vm.snapshot.paused", pauseIoId);

        // While snapshot-paused, the mic producer may continue writing into the ring. Advance
        // `write_pos` again to model time passing / samples being produced.
        Atomics.store(header, MIC_WRITE_POS_INDEX, 2000);

        const resumeCpuId = nextId++;
        cpu.postMessage({ kind: "vm.snapshot.resume", requestId: resumeCpuId });
        await waitFor(cpu, "vm.snapshot.resumed", resumeCpuId);

        const resumeIoId = nextId++;
        io.postMessage({ kind: "vm.snapshot.resume", requestId: resumeIoId });
        await waitFor(io, "vm.snapshot.resumed", resumeIoId);

        const readAfterResume = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
        const writeAfterResume = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;

        return { readBeforePause, writeBeforePause, readAfterResume, writeAfterResume };
      };

      return {
        io: await runCycle("io"),
        cpu: await runCycle("cpu"),
      };
    },
    { MIC_HEADER_U32_LEN, MIC_READ_POS_INDEX, MIC_WRITE_POS_INDEX },
  );

  const assertDiscarded = (rec: { readBeforePause: number; writeBeforePause: number; readAfterResume: number; writeAfterResume: number }) => {
    // Ensure the ring actually had a backlog before the pause/resume cycle.
    expect(rec.readBeforePause).toBeLessThan(rec.writeBeforePause);
    // On resume, the active mic consumer should discard any buffered samples so capture resumes
    // from the most recent audio.
    expect(rec.readAfterResume).toBe(rec.writeAfterResume);
  };

  assertDiscarded(result.io);
  assertDiscarded(result.cpu);
});
