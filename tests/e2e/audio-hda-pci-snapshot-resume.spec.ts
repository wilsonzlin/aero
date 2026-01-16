import { expect, test } from "@playwright/test";

import { waitForAudioOutputNonSilent } from "./util/audio";
import { probeOpfsSyncAccessHandle, removeOpfsEntryBestEffort } from "./util/opfs";

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

  // Worker status flips to READY before the background WASM initialization finishes. Snapshot save/restore
  // requires CPU+IO WASM exports (`WasmVm.save_state_v2` + IO-side snapshot container writers), so wait
  // for WASM_READY before attempting the snapshot.
  await page.waitForFunction(
    () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const wc = (globalThis as any).__aeroWorkerCoordinator;
      if (!wc || typeof wc.getWorkerWasmStatus !== "function") return false;
      return Boolean(wc.getWorkerWasmStatus("cpu")) && Boolean(wc.getWorkerWasmStatus("io"));
    },
    undefined,
    { timeout: 120_000 },
  );

  const initialIndices = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as { readIndex: Uint32Array; writeIndex: Uint32Array };
    return {
      read: Atomics.load(ring.readIndex, 0) >>> 0,
      write: Atomics.load(ring.writeIndex, 0) >>> 0,
    };
  });
  expect(initialIndices).not.toBeNull();
  const initialRead = initialIndices!.read;
  const initialWrite = initialIndices!.write;

  // Sanity check: ensure the AudioWorklet is actually consuming frames before snapshotting.
  await page.waitForFunction(
    (initialRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { readIndex: Uint32Array };
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      return ((read - (initialRead as number)) >>> 0) > 0;
    },
    initialRead,
    { timeout: 20_000 },
  );

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

  // Ensure the producer is writing actual (non-silent) samples into the ring, not just
  // advancing indices.
  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputHdaPciDevice", { threshold: 0.01, timeoutMs: 20_000 });

  // Ignore any startup underruns while the worker/runtime + PCI device pipeline bootstraps;
  // assert on the *delta* over a steady-state window so this stays robust on cold CI runners.
  const steady0 = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;
    return {
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });
  expect(steady0).not.toBeNull();

  // Let the system run for a bit so we catch sustained underruns (not just “it started once”).
  await page.waitForTimeout(1000);

  const beforeSnapshot = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const backend = (globalThis as any).__aeroAudioToneBackend;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as { readIndex: Uint32Array; writeIndex: Uint32Array };
    const read = Atomics.load(ring.readIndex, 0) >>> 0;
    const write = Atomics.load(ring.writeIndex, 0) >>> 0;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      backend,
      read,
      write,
      bufferLevelFrames: typeof out?.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : null,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });

  expect(beforeSnapshot).not.toBeNull();
  expect(beforeSnapshot!.enabled).toBe(true);
  expect(beforeSnapshot!.state).toBe("running");
  expect(beforeSnapshot!.backend).toBe("io-worker-hda-pci");
  expect(beforeSnapshot!.bufferLevelFrames).not.toBeNull();
  expect(beforeSnapshot!.bufferLevelFrames as number).toBeGreaterThan(0);
  const deltaUnderrun = (((beforeSnapshot!.underruns as number) - (steady0!.underruns as number)) >>> 0) as number;
  const deltaOverrun = (((beforeSnapshot!.overruns as number) - (steady0!.overruns as number)) >>> 0) as number;
  expect(deltaOverrun).toBe(0);
  // Underruns are tracked in frames. Allow a few render quanta of slack over the window
  // (covers occasional scheduling jitter while still catching sustained underruns).
  expect(deltaUnderrun).toBeLessThanOrEqual(1024);

  // Generate a per-test snapshot path to avoid collisions when Playwright runs specs in parallel.
  const snapshotPath = `state/playwright-hda-pci-snapshot-${Date.now()}-${Math.random().toString(16).slice(2)}.snap`;

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
      const msg = err instanceof Error ? err.message : err;
      const error = String(msg ?? "Error")
        .replace(/[\\x00-\\x1F\\x7F]/g, " ")
        .replace(/\\s+/g, " ")
        .trim()
        .slice(0, 512);
      return { ok: false as const, error };
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
        // Preserve the exact original postMessage method and descriptor so cleanup can fully
        // restore the Worker object shape (avoid leaving an own `postMessage` property behind).
        const originalPostMessage = io.postMessage;
        const originalPostMessageDescriptor = Object.getOwnPropertyDescriptor(io, "postMessage");
        let postMessageWrapped = false;
        let postMessageRestorePending = false;

        const cleanup = () => {
          io.removeEventListener("message", onMessage as EventListener);
          if (postMessageRestorePending) {
            try {
              if (originalPostMessageDescriptor) {
                Object.defineProperty(io, "postMessage", originalPostMessageDescriptor);
              } else {
                // We created an own property when wrapping; delete it to fall back to Worker.prototype.postMessage.
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                delete (io as any).postMessage;
              }
            } catch {
              try {
                // Restore the original postMessage implementation so we don't interfere with subsequent coordinator ops.
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                (io as any).postMessage = originalPostMessage;
              } catch {
                // ignore best-effort
              }
            }
          }
          clearTimeout(timeout);
        };

        const fail = (err: unknown) => {
          if (resolved) return;
          resolved = true;
          cleanup();
          const msg = err instanceof Error ? err.message : err;
          const error = String(msg ?? "Error")
            .replace(/[\\x00-\\x1F\\x7F]/g, " ")
            .replace(/\\s+/g, " ")
            .trim()
            .slice(0, 512);
          resolve({ ok: false as const, error });
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

  // Confirm the AudioWorklet resumes consuming frames after restore (read index advances).
  await page.waitForFunction(
    (baselineRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { readIndex: Uint32Array };
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      return ((read - (baselineRead as number)) >>> 0) > 0;
    },
    afterRestore!.read,
    { timeout: 20_000 },
  );

  // Ensure the IO-worker HDA device is producing actual (non-silent) samples after restore.
  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputHdaPciDevice", { threshold: 0.01 });

  await removeOpfsEntryBestEffort(page, snapshotPath);
});
