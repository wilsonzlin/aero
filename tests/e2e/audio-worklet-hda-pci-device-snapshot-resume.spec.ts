import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("IO-worker HDA PCI audio does not fast-forward after worker snapshot restore", async ({ page }) => {
  test.setTimeout(180_000);
  test.skip(test.info().project.name !== "chromium", "Snapshot + AudioWorklet test only runs on Chromium.");

  // The HDA PCI demo boots multiple workers and may require a cold WASM compile in CI.
  page.setDefaultTimeout(120_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-hda-pci-device");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    return out?.enabled === true && out?.context?.state === "running";
  });

  // Ensure the worker runtime is fully ready before snapshotting (snapshot requires NET too).
  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const wc = (globalThis as any).__aeroWorkerCoordinator;
    if (!wc || typeof wc.getWorkerStatuses !== "function") return false;
    const statuses = wc.getWorkerStatuses();
    return statuses?.cpu?.state === "ready" && statuses?.io?.state === "ready" && statuses?.net?.state === "ready";
  });

  const snapshotSupported = await page.evaluate(() => {
    const g = globalThis as unknown as { FileSystemFileHandle?: unknown };
    const ctor = g.FileSystemFileHandle as { prototype?: { createSyncAccessHandle?: unknown } } | undefined;
    return typeof ctor?.prototype?.createSyncAccessHandle === "function";
  });
  if (!snapshotSupported) {
    // Some Chromium variants/embeds do not expose OPFS SyncAccessHandle; skip rather than failing.
    return;
  }

  const snapshotPath = `state/playwright-hda-pci-snapshot-${Date.now()}.snap`;

  const saveResult = await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const wc = (globalThis as any).__aeroWorkerCoordinator;
    if (!wc) return { ok: false, error: "Missing __aeroWorkerCoordinator global." };
    try {
      await wc.snapshotSaveToOpfs(path);
      return { ok: true as const };
    } catch (err) {
      return { ok: false as const, error: err instanceof Error ? err.message : String(err) };
    }
  }, snapshotPath);

  if (!saveResult.ok) {
    // Best-effort: tolerate environments where snapshots are compiled out / unavailable.
    expect(saveResult.error).toContain("unavailable");
    return;
  }

  // Simulate time passing between save and restore.
  await page.waitForTimeout(1500);

  const restoreResult = await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const wc = (globalThis as any).__aeroWorkerCoordinator;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;

    if (!wc) return { ok: false as const, error: "Missing __aeroWorkerCoordinator global." };
    if (!out?.enabled) return { ok: false as const, error: "Missing __aeroAudioOutputHdaPciDevice output." };

    const cpu = wc.getWorker?.("cpu");
    const io = wc.getWorker?.("io");
    const net = wc.getWorker?.("net");
    if (!cpu || !io || !net) {
      return { ok: false as const, error: "Missing CPU/IO/NET worker instances." };
    }

    // Inline snapshot RPC helper (mirrors `WorkerCoordinator.snapshotRpc`).
    // Use a high requestId base to avoid colliding with any coordinator-driven snapshot ops.
    let nextRequestId = 1_000_000;
    const rpc = async (
      worker: Worker,
      request: Record<string, unknown>,
      expectedKind: string,
      opts: { timeoutMs: number; transfer?: Transferable[] },
    ): Promise<any> => {
      const requestId = nextRequestId++;
      const msg = { ...request, requestId };
      return await new Promise((resolve, reject) => {
        const onMessage = (ev: MessageEvent<unknown>) => {
          const data = ev.data as { kind?: unknown; requestId?: unknown };
          if (!data || typeof data !== "object") return;
          if (data.kind !== expectedKind) return;
          if (data.requestId !== requestId) return;
          cleanup();
          resolve(ev.data);
        };
        const cleanup = () => {
          worker.removeEventListener("message", onMessage as EventListener);
          clearTimeout(timer);
        };
        const timer = setTimeout(() => {
          cleanup();
          reject(new Error(`Timed out waiting for ${expectedKind} (requestId=${requestId})`));
        }, opts.timeoutMs);
        worker.addEventListener("message", onMessage as EventListener);
        try {
          if (opts.transfer && opts.transfer.length) {
            worker.postMessage(msg, opts.transfer);
          } else {
            worker.postMessage(msg);
          }
        } catch (err) {
          cleanup();
          reject(err instanceof Error ? err : new Error(String(err)));
        }
      });
    };

    try {
      // Pause all participants (match coordinator ordering: guest/device side first).
      await rpc(cpu, { kind: "vm.snapshot.pause" }, "vm.snapshot.paused", { timeoutMs: 5_000 });
      await rpc(io, { kind: "vm.snapshot.pause" }, "vm.snapshot.paused", { timeoutMs: 5_000 });
      await rpc(net, { kind: "vm.snapshot.pause" }, "vm.snapshot.paused", { timeoutMs: 5_000 });

      // Restore snapshot bytes + apply device state (IO worker owns device instances, including HDA).
      const restored = (await rpc(io, { kind: "vm.snapshot.restoreFromOpfs", path }, "vm.snapshot.restored", {
        timeoutMs: 120_000,
      })) as { ok: boolean; cpu?: ArrayBuffer; mmu?: ArrayBuffer; error?: unknown };
      if (!restored.ok) {
        const err = restored.error as { message?: unknown } | undefined;
        return { ok: false as const, error: typeof err?.message === "string" ? err.message : "restoreFromOpfs failed" };
      }

      const cpuBuf = restored.cpu;
      const mmuBuf = restored.mmu;
      if (!(cpuBuf instanceof ArrayBuffer) || !(mmuBuf instanceof ArrayBuffer)) {
        return { ok: false as const, error: "restoreFromOpfs returned unexpected payload (missing cpu/mmu)." };
      }

      const cpuSet = (await rpc(cpu, { kind: "vm.snapshot.setCpuState", cpu: cpuBuf, mmu: mmuBuf }, "vm.snapshot.cpuStateSet", {
        timeoutMs: 10_000,
        transfer: [cpuBuf, mmuBuf],
      })) as { ok: boolean; error?: unknown };
      if (!cpuSet.ok) {
        const err = cpuSet.error as { message?: unknown } | undefined;
        return { ok: false as const, error: typeof err?.message === "string" ? err.message : "setCpuState failed" };
      }

      // Baseline ring indices while workers are *still paused*, so no device tick can run yet.
      const sr = typeof out?.context?.sampleRate === "number" ? (out.context.sampleRate as number) : 48_000;
      const ring = out.ringBuffer as { readIndex: Uint32Array; writeIndex: Uint32Array };
      const read0 = Atomics.load(ring.readIndex, 0) >>> 0;
      const write0 = Atomics.load(ring.writeIndex, 0) >>> 0;
      const t0 = performance.now();

      // Resume workers and measure the immediate post-resume write delta.
      const cpuResume = rpc(cpu, { kind: "vm.snapshot.resume" }, "vm.snapshot.resumed", { timeoutMs: 5_000 });
      const ioResume = rpc(io, { kind: "vm.snapshot.resume" }, "vm.snapshot.resumed", { timeoutMs: 5_000 });

      // Give the IO tick loop a moment to run. Keep this well below the HDA tick max-delta clamp (100ms),
      // otherwise a regression could hide inside the expected-window math.
      await new Promise((resolve) => setTimeout(resolve, 40));

      const t1 = performance.now();
      const read1 = Atomics.load(ring.readIndex, 0) >>> 0;
      const write1 = Atomics.load(ring.writeIndex, 0) >>> 0;

      // Resume NET after CPU/IO (matches coordinator ordering to avoid shared-ring races).
      await Promise.allSettled([cpuResume, ioResume]);
      const netResume = rpc(net, { kind: "vm.snapshot.resume" }, "vm.snapshot.resumed", { timeoutMs: 5_000 });
      await Promise.allSettled([netResume]);

      const elapsedMs = t1 - t0;
      const writeDelta = ((write1 - write0) >>> 0) as number;
      const readDelta = ((read1 - read0) >>> 0) as number;

      // Allow some scheduler slop, but the producer must not "catch up" by writing ~100ms worth of
      // frames immediately after resume (the symptom we want to prevent).
      const slackMs = 20;
      const maxWriteDelta = Math.ceil((sr * Math.max(0, elapsedMs + slackMs)) / 1000);

      return { ok: true as const, sr, elapsedMs, read0, read1, readDelta, write0, write1, writeDelta, maxWriteDelta };
    } catch (err) {
      return { ok: false as const, error: err instanceof Error ? err.message : String(err) };
    }
  }, snapshotPath);

  if (!restoreResult.ok) {
    // Best-effort: tolerate environments where snapshot restore is compiled out / unavailable.
    if (typeof restoreResult.error === "string" && restoreResult.error.includes("unavailable")) {
      expect(restoreResult.error).toContain("unavailable");
      return;
    }
    throw new Error(`snapshot restore sequence failed: ${String(restoreResult.error)}`);
  }

  // The producer must not burst-write significantly more than real-time audio immediately after restore.
  expect(restoreResult.writeDelta).toBeLessThanOrEqual(restoreResult.maxWriteDelta);

  // Sanity: the device should still be producing audio after restore.
  await page.waitForFunction(
    (write0) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const write = Atomics.load(out.ringBuffer.writeIndex, 0) >>> 0;
      return ((write - (write0 as number)) >>> 0) > 0;
    },
    restoreResult.write1,
    { timeout: 10_000 },
  );
});
