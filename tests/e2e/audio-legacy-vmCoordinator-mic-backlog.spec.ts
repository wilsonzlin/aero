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

test("Legacy VmCoordinator discards buffered mic samples on attach/resume/step (stale latency avoidance)", async ({ page }) => {
  test.setTimeout(90_000);
  test.skip(test.info().project.name !== "chromium", "Mic ring SharedArrayBuffer tests only run on Chromium.");
  page.setDefaultTimeout(90_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  // Start the legacy single-worker VM coordinator (src/emulator/vmCoordinator.js) via the harness UI.
  await page.click("#vm-start-coop");

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const vm = (globalThis as any).__aeroVm as { state?: unknown } | undefined;
    return vm?.state === "running";
  });

  const result = await page.evaluate(
    async ({
      MIC_CAPACITY_SAMPLES_INDEX,
      MIC_DROPPED_SAMPLES_INDEX,
      MIC_HEADER_BYTES,
      MIC_HEADER_U32_LEN,
      MIC_READ_POS_INDEX,
      MIC_WRITE_POS_INDEX,
    }) => {
      const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
      const waitUntil = async (pred: () => boolean, timeoutMs: number, intervalMs = 10) => {
        const deadline = Date.now() + timeoutMs;
        while (Date.now() < deadline) {
          if (pred()) return;
          await sleep(intervalMs);
        }
        throw new Error(`Timed out waiting for condition (${timeoutMs}ms)`);
      };

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const vm = (globalThis as any).__aeroVm as any;
      if (!vm) throw new Error("Missing globalThis.__aeroVm");

      // Pause so the CPU worker is not consuming. This lets us distinguish "consumer drained backlog"
      // from the intended policy ("discard backlog via readPos := writePos").
      await vm.pause();

      const capacitySamples = 262_144; // 2^18, enough headroom for a large backlog.
      const sab = new SharedArrayBuffer(MIC_HEADER_BYTES + capacitySamples * Float32Array.BYTES_PER_ELEMENT);
      const header = new Uint32Array(sab, 0, MIC_HEADER_U32_LEN);

      // Simulate the host-side mic producer writing a large backlog while the VM is paused / before attach.
      const initialWritePos = 200_000;
      Atomics.store(header, MIC_WRITE_POS_INDEX, initialWritePos >>> 0);
      Atomics.store(header, MIC_READ_POS_INDEX, 0);
      Atomics.store(header, MIC_DROPPED_SAMPLES_INDEX, 0);
      Atomics.store(header, MIC_CAPACITY_SAMPLES_INDEX, capacitySamples >>> 0);

      // Attach mic ring while still paused; the worker should discard any buffered samples immediately.
      vm.setMicrophoneRingBuffer(sab, { sampleRate: 48_000 });

      await waitUntil(() => Atomics.load(header, MIC_READ_POS_INDEX) === Atomics.load(header, MIC_WRITE_POS_INDEX), 2_000);

      const readAfterAttach = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
      const writeAfterAttach = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;

      // While paused, advance writePos to model continued mic capture, then resume.
      const writeBeforeResume = (writeAfterAttach + 200_000) >>> 0;
      Atomics.store(header, MIC_WRITE_POS_INDEX, writeBeforeResume);
      const readBeforeResume = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;

      await vm.resume();

      const readAfterResume = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
      const writeAfterResume = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;

      // Pause again, model more buffered samples, then step.
      await vm.pause();

      const writeBeforeStep = (writeAfterResume + 200_000) >>> 0;
      Atomics.store(header, MIC_WRITE_POS_INDEX, writeBeforeStep);
      const readBeforeStep = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;

      await vm.step();

      const readAfterStep = Atomics.load(header, MIC_READ_POS_INDEX) >>> 0;
      const writeAfterStep = Atomics.load(header, MIC_WRITE_POS_INDEX) >>> 0;

      // Clean up so this test doesn't leave workers running.
      vm.setMicrophoneRingBuffer(null);
      vm.shutdown();

      return {
        attach: { readAfterAttach, writeAfterAttach },
        resume: { readBeforeResume, writeBeforeResume, readAfterResume, writeAfterResume },
        step: { readBeforeStep, writeBeforeStep, readAfterStep, writeAfterStep },
      };
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

  // Attach should discard backlog while paused.
  expect(result.attach.readAfterAttach).toBe(result.attach.writeAfterAttach);

  // Resume should discard backlog produced while paused.
  expect(result.resume.readBeforeResume).toBeLessThan(result.resume.writeBeforeResume);
  expect(result.resume.readAfterResume).toBe(result.resume.writeAfterResume);

  // Step should also treat the pause boundary as a discard point (avoid replaying stale mic).
  expect(result.step.readBeforeStep).toBeLessThan(result.step.writeBeforeStep);
  expect(result.step.readAfterStep).toBe(result.step.writeAfterStep);
});

