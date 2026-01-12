import { expect, test } from "@playwright/test";

import { probeOpfsSyncAccessHandle } from "./util/opfs";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("IO-worker HDA PCI audio does not fast-forward after worker snapshot restore", async ({ page }) => {
  // HDA PCI audio exercises the full worker runtime + IO-worker WASM snapshot pipeline (uncached in CI).
  test.setTimeout(240_000);
  test.skip(test.info().project.name !== "chromium", "Snapshot + AudioWorklet test only runs on Chromium.");

  page.setDefaultTimeout(120_000);

  // We only need an origin context for the OPFS capability probe; avoid waiting for full page load.
  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "domcontentloaded" });

  const snapshotSupport = await probeOpfsSyncAccessHandle(page);

  if (!snapshotSupport.ok || !snapshotSupport.supported) {
    test.skip(
      true,
      snapshotSupport.ok
        ? `OPFS SyncAccessHandle unsupported in this browser/context (${snapshotSupport.reason ?? "unknown reason"}).`
        : `Failed to probe OPFS SyncAccessHandle support (${snapshotSupport.reason ?? "unknown error"}).`,
    );
  }

  // Coordinator is exposed by the repo-root harness (`src/main.ts`).
  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return !!(globalThis as any).__aeroWorkerCoordinator;
  });

  await page.click("#init-audio-hda-pci-device");

  await page.waitForFunction(
    () => {
      // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      return out?.enabled === true && out?.context?.state === "running";
    },
    undefined,
    // Full IO-worker WASM init + PCI enumeration can be slow on cold CI runners.
    { timeout: 120_000 },
  );

  // Ensure the worker runtime is fully ready before snapshotting.
  await page.waitForFunction(
    () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const wc = (globalThis as any).__aeroWorkerCoordinator;
      if (!wc || typeof wc.getWorkerStatuses !== "function") return false;
      const statuses = wc.getWorkerStatuses();
      return statuses?.cpu?.state === "ready" && statuses?.io?.state === "ready" && statuses?.net?.state === "ready";
    },
    undefined,
    { timeout: 120_000 },
  );

  const initialWrite = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as { writeIndex: Uint32Array };
    return Atomics.load(ring.writeIndex, 0) >>> 0;
  });
  expect(initialWrite).not.toBeNull();

  // Confirm the IO worker is producing into the ring buffer before snapshotting.
  await page.waitForFunction(
    (initialWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (initialWrite as number)) >>> 0) > 0;
    },
    initialWrite,
    { timeout: 60_000 },
  );

  const snapshotPath = `state/playwright-hda-pci-snapshot-${Date.now()}.snap`;

  // Save snapshot via coordinator (pause → save → resume).
  const saveResult = await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator;
    if (!coord || typeof coord.snapshotSaveToOpfs !== "function") {
      return { ok: false as const, error: "Missing __aeroWorkerCoordinator.snapshotSaveToOpfs()" };
    }
    try {
      await coord.snapshotSaveToOpfs(path);
      return { ok: true as const };
    } catch (err) {
      return { ok: false as const, error: err instanceof Error ? err.message : String(err) };
    }
  }, snapshotPath);

  if (!saveResult.ok) {
    // Best-effort: tolerate dev builds where snapshot exports are compiled out.
    if (typeof saveResult.error === "string" && saveResult.error.includes("unavailable")) {
      test.skip(true, `VM snapshot save unavailable in this build (${saveResult.error}).`);
    }
    throw new Error(`snapshot save failed: ${String(saveResult.error)}`);
  }

  // Simulate time passing between save and restore (user delay, slow restore, etc.).
  // Keep this > the IO-worker HDA max-delta clamp (100ms) so a regression will still manifest.
  await page.waitForTimeout(500);

  // Restore snapshot via coordinator (pause → restore → resume), while measuring the immediate
  // post-resume write delta to catch any producer burst/catch-up.
  const restoreResult = await page.evaluate(
    async ({ path, sampleWindowMs, slackMs }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const coord = (globalThis as any).__aeroWorkerCoordinator;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;

      if (!coord || typeof coord.snapshotRestoreFromOpfs !== "function") {
        return { ok: false as const, error: "Missing __aeroWorkerCoordinator.snapshotRestoreFromOpfs()" };
      }
      if (!out?.enabled) {
        return { ok: false as const, error: "Missing __aeroAudioOutputHdaPciDevice output." };
      }

      const io = coord.getWorker?.("io");
      if (!io) {
        return { ok: false as const, error: "Missing IO worker (getWorker('io') returned null)." };
      }

      const sr = typeof out?.context?.sampleRate === "number" ? (out.context.sampleRate as number) : 48_000;
      const ring = out.ringBuffer as { readIndex: Uint32Array; writeIndex: Uint32Array };

      type SnapshotMsg = { kind?: unknown; ok?: unknown; error?: { message?: unknown } };

      return await new Promise<
        | ({
            ok: true;
            sr: number;
            restoredRead: number;
            restoredWrite: number;
            resumedRead0: number;
            resumedWrite0: number;
            resumedAtMs: number;
            read1: number;
            write1: number;
            elapsedMs: number;
            writeDelta: number;
            maxWriteDelta: number;
          } & Record<string, never>)
        | { ok: false; error: string }
      >((resolve) => {
        let resolved = false;
        let restorePromise: Promise<void> | null = null;
        let restoredRead: number | null = null;
        let restoredWrite: number | null = null;
        let resumeSentAtMs: number | null = null;
        let resumedAtMs: number | null = null;
        let resumedRead0: number | null = null;
        let resumedWrite0: number | null = null;
        let burstBaselineWrite: number | null = null;
        let burstStartAtMs: number | null = null;
        let burstSampleScheduled = false;
        // Preserve the exact original method so cleanup restores the same function identity.
        const originalPostMessage = io.postMessage;
        let postMessageWrapped = false;
        let postMessageRestorePending = false;

        const cleanup = () => {
          io.removeEventListener("message", onMessage as EventListener);
          if (postMessageRestorePending) {
            try {
              // Restore the original postMessage implementation so we don't interfere with subsequent coordinator ops.
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              (io as any).postMessage = originalPostMessage;
            } catch {
              // ignore best-effort
            }
          }
          clearTimeout(timeout);
        };

        const fail = (err: unknown) => {
          if (resolved) return;
          resolved = true;
          cleanup();
          resolve({ ok: false as const, error: err instanceof Error ? err.message : String(err) });
        };

        const timeout = setTimeout(() => fail(new Error("Timed out waiting for snapshot restore burst probe.")), 180_000);

        const scheduleBurstSample = (startAtMs: number, baselineWrite: number) => {
          if (burstSampleScheduled) return;
          burstSampleScheduled = true;
          burstStartAtMs = startAtMs;
          burstBaselineWrite = baselineWrite;
          setTimeout(() => {
            if (resolved) return;
            if (burstStartAtMs === null || burstBaselineWrite === null) {
              fail(new Error("Internal error: missing burst sample state."));
              return;
            }
            const t1 = performance.now();
            const read1 = Atomics.load(ring.readIndex, 0) >>> 0;
            const write1 = Atomics.load(ring.writeIndex, 0) >>> 0;
            const elapsedMs = t1 - burstStartAtMs;
            const writeDelta = ((write1 - burstBaselineWrite) >>> 0) as number;
            const maxWriteDelta = Math.ceil((sr * Math.max(0, elapsedMs + slackMs)) / 1000);

            const payload = {
              ok: true as const,
              sr,
              restoredRead: (restoredRead ?? 0) >>> 0,
              restoredWrite: (restoredWrite ?? 0) >>> 0,
              resumedRead0: (resumedRead0 ?? 0) >>> 0,
              resumedWrite0: (resumedWrite0 ?? 0) >>> 0,
              resumedAtMs: resumedAtMs ?? burstStartAtMs,
              read1,
              write1,
              elapsedMs,
              writeDelta,
              maxWriteDelta,
            };

            const finishOk = () => {
              if (resolved) return;
              resolved = true;
              cleanup();
              resolve(payload);
            };

            const pending = restorePromise;
            if (!pending) {
              finishOk();
              return;
            }

            pending.then(finishOk).catch((err) => fail(err));
          }, sampleWindowMs);
        };

        // Intercept the coordinator's `vm.snapshot.resume` request so we can start our measurement window
        // before the IO worker has a chance to tick and potentially burst-write.
        try {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (io as any).postMessage = (message: unknown, transfer?: unknown) => {
            try {
              const rec = message as { kind?: unknown } | null;
              if (rec && typeof rec === "object" && rec.kind === "vm.snapshot.resume" && resumeSentAtMs === null) {
                resumeSentAtMs = performance.now();
                if (typeof restoredWrite === "number") {
                  scheduleBurstSample(resumeSentAtMs, restoredWrite);
                }
              }
            } catch {
              // ignore
            }
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            return (originalPostMessage as any).call(io, message as any, transfer as any);
          };
          postMessageWrapped = true;
          postMessageRestorePending = true;
        } catch {
          postMessageWrapped = false;
        }

        const onMessage = (ev: MessageEvent<unknown>) => {
          const data = ev.data as SnapshotMsg | null;
          if (!data || typeof data !== "object") return;
          if (data.kind === "vm.snapshot.restored") {
            if (data.ok !== true) {
              const msg = typeof data.error?.message === "string" ? data.error.message : "vm.snapshot.restored failed";
              fail(new Error(msg));
              return;
            }
            restoredRead = Atomics.load(ring.readIndex, 0) >>> 0;
            restoredWrite = Atomics.load(ring.writeIndex, 0) >>> 0;
            // In the normal (non-failing) flow, the coordinator has not yet resumed the VM.
            // If we already observed the resume request, schedule the burst sample now.
            if (resumeSentAtMs !== null && typeof restoredWrite === "number") {
              scheduleBurstSample(resumeSentAtMs, restoredWrite);
            }
            return;
          }

          if (data.kind === "vm.snapshot.resumed") {
            // Ignore resumes not associated with our restore (e.g. earlier snapshot ops).
            if (restoredWrite === null || restoredRead === null) return;
            if (data.ok !== true) {
              const msg = typeof data.error?.message === "string" ? data.error.message : "vm.snapshot.resumed failed";
              fail(new Error(msg));
              return;
            }
            if (resumedAtMs !== null) return;
            resumedAtMs = performance.now();
            resumedRead0 = Atomics.load(ring.readIndex, 0) >>> 0;
            resumedWrite0 = Atomics.load(ring.writeIndex, 0) >>> 0;

            // Fallback: if we couldn't intercept the `vm.snapshot.resume` request (e.g. postMessage is non-writable),
            // start the burst window at the time we observe `vm.snapshot.resumed`.
            //
            // This is slightly weaker (a burst could occur before this event is delivered), but still catches many
            // regressions and keeps the test from hanging.
            if (!burstSampleScheduled && typeof resumedWrite0 === "number") {
              scheduleBurstSample(resumedAtMs, resumedWrite0);
            }
          }
        };

        io.addEventListener("message", onMessage as EventListener);

        try {
          restorePromise = coord.snapshotRestoreFromOpfs(path);
          restorePromise.catch((err: unknown) => fail(err));
        } catch (err) {
          fail(err);
        }

        // If we fail to wrap postMessage, make sure we don't accidentally leave it overridden.
        if (!postMessageWrapped) {
          postMessageRestorePending = false;
        }
      });
    },
    // Allow some scheduler jitter when measuring the immediate post-resume window.
    { path: snapshotPath, sampleWindowMs: 40, slackMs: 20 },
  );

  if (!restoreResult.ok) {
    // Best-effort: tolerate dev builds where snapshot exports are compiled out.
    if (typeof restoreResult.error === "string" && restoreResult.error.includes("unavailable")) {
      test.skip(true, `VM snapshot restore unavailable in this build (${restoreResult.error}).`);
    }
    throw new Error(`snapshot restore failed: ${String(restoreResult.error)}`);
  }

  // The producer must not "fast-forward" by writing significantly more than real-time audio
  // immediately after restore/resume.
  expect(restoreResult.writeDelta).toBeLessThanOrEqual(restoreResult.maxWriteDelta);

  // Give the workers a moment to tick and begin producing again.
  await page.waitForTimeout(250);

  const afterRestore = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as {
      readIndex: Uint32Array;
      writeIndex: Uint32Array;
      underrunCount: Uint32Array;
      overrunCount: Uint32Array;
      capacityFrames: number;
    };
    return {
      state: out?.context?.state ?? null,
      read: Atomics.load(ring.readIndex, 0) >>> 0,
      write: Atomics.load(ring.writeIndex, 0) >>> 0,
      underrun: Atomics.load(ring.underrunCount, 0) >>> 0,
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
      capacity: ring.capacityFrames as number,
    };
  });
  expect(afterRestore).not.toBeNull();
  expect(afterRestore!.state).toBe("running");

  // Confirm the producer resumes after restore (write index advances).
  await page.waitForFunction(
    (baselineWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (baselineWrite as number)) >>> 0) > 0;
    },
    afterRestore!.write,
    { timeout: 60_000 },
  );

  // Ensure the IO-worker HDA device is producing actual (non-silent) samples after restore.
  await page.waitForFunction(
    () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.ringBuffer?.samples || !out?.ringBuffer?.writeIndex) return false;
      const samples: Float32Array = out.ringBuffer.samples;
      const writeIndex: Uint32Array = out.ringBuffer.writeIndex;
      const cc = out.ringBuffer.channelCount | 0;
      const cap = out.ringBuffer.capacityFrames | 0;
      if (cc <= 0 || cap <= 0) return false;
      const write = Atomics.load(writeIndex, 0) >>> 0;
      const framesToInspect = Math.min(1024, cap);
      const startFrame = (write - framesToInspect) >>> 0;
      let maxAbs = 0;
      for (let i = 0; i < framesToInspect; i++) {
        const frame = (startFrame + i) % cap;
        const base = frame * cc;
        for (let c = 0; c < cc; c++) {
          const s = samples[base + c] ?? 0;
          const a = Math.abs(s);
          if (a > maxAbs) maxAbs = a;
        }
      }
      return maxAbs > 0.01;
    },
    { timeout: 10_000 },
  );

  // Best-effort cleanup: OPFS can persist across runs in some environments.
  // Ignore failures (missing APIs, already deleted, permission issues, etc.).
  await page.evaluate(async (path) => {
    try {
      const storage = (navigator as Navigator & { storage?: StorageManager | undefined }).storage;
      const getDir = (storage as StorageManager & { getDirectory?: unknown })?.getDirectory as
        | ((this: StorageManager) => Promise<FileSystemDirectoryHandle>)
        | undefined;
      if (typeof getDir !== "function") return;

      const parts = String(path)
        .split("/")
        .map((p) => p.trim())
        .filter((p) => p.length > 0);
      if (parts.length === 0) return;
      const filename = parts.pop();
      if (!filename) return;

      let dir = await getDir.call(storage);
      for (const part of parts) {
        dir = await dir.getDirectoryHandle(part);
      }
      await dir.removeEntry(filename);
    } catch {
      // ignore
    }
  }, snapshotPath);
});
