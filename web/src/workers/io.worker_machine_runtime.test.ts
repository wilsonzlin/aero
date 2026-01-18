import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { unrefBestEffort } from "../unrefSafe";
import { openRingByKind } from "../ipc/ipc";
import { encodeCommand } from "../ipc/protocol";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import {
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  createIoIpcSab,
  createSharedMemoryViews,
  StatusIndex,
  type SharedMemorySegments,
} from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { WORKER_THREADS_WEBWORKER_EXEC_ARGV } from "./test_utils/worker_exec_argv";

const WORKER_EXEC_ARGV = WORKER_THREADS_WEBWORKER_EXEC_ARGV;

function arraysEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  for (let i = 0; i < a.byteLength; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

async function waitForWorkerMessage(worker: Worker, predicate: (msg: unknown) => boolean, timeoutMs: number): Promise<unknown> {
  return new Promise((resolve, reject) => {
    // Worker thread startup + module evaluation can be slow under heavy CI load.
    // Add slack to keep these integration tests from flaking when the suite is
    // running in parallel.
    const effectiveTimeoutMs = timeoutMs * 2;
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${effectiveTimeoutMs}ms waiting for worker message`));
    }, effectiveTimeoutMs);
    unrefBestEffort(timer);

    const onMessage = (msg: unknown) => {
      const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
      if (maybeProtocol?.type === MessageType.ERROR) {
        cleanup();
        const errMsg = typeof maybeProtocol.message === "string" ? maybeProtocol.message : "";
        reject(new Error(`worker reported error${errMsg ? `: ${errMsg}` : ""}`));
        return;
      }
      let matched = false;
      try {
        matched = predicate(msg);
      } catch (err) {
        cleanup();
        reject(err instanceof Error ? err : new Error(String(err)));
        return;
      }
      if (!matched) return;
      cleanup();
      resolve(msg);
    };

    const onError = (err: unknown) => {
      cleanup();
      reject(err instanceof Error ? err : new Error(String(err)));
    };

    const onExit = (code: number) => {
      cleanup();
      reject(new Error(`worker exited before emitting the expected message (code=${code})`));
    };

    function cleanup(): void {
      clearTimeout(timer);
      worker.off("message", onMessage);
      worker.off("error", onError);
      worker.off("exit", onExit);
    }

    worker.on("message", onMessage);
    worker.on("error", onError);
    worker.on("exit", onExit);
  });
}

function makeMachineConfig(): AeroConfig {
  return {
    guestMemoryMiB: 1,
    vramMiB: 16,
    enableWorkers: true,
    enableWebGPU: false,
    proxyUrl: null,
    activeDiskImage: null,
    logLevel: "info",
    vmRuntime: "machine",
  };
}

function makeInit(segments: SharedMemorySegments): WorkerInitMessage {
  return {
    kind: "init",
    role: "io",
    controlSab: segments.control,
    guestMemory: segments.guestMemory,
    vram: segments.vram,
    vgaFramebuffer: segments.sharedFramebuffer,
    scanoutState: segments.scanoutState,
    scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    cursorState: segments.cursorState,
    cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
    ioIpcSab: segments.ioIpc,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
  };
}

describe("workers/io.worker (machine runtime host-only mode)", () => {
  it("reaches READY without initializing device models and does not write guest RAM", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpc: createIoIpcSab({ includeNet: false, includeHidIn: false }),
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    // Fill guest RAM with a sentinel pattern and verify it remains unchanged.
    // (Guest RAM is small in this unit test; for larger guests this would be too costly.)
    views.guestU8.fill(0x5a);
    const guestCopy = views.guestU8.slice();

    const worker = new Worker(new URL("./io.worker.ts", import.meta.url), {
      type: "module",
      execArgv: WORKER_EXEC_ARGV,
    } as unknown as WorkerOptions);

    try {
      const readyPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "io",
        5000,
      );

      worker.postMessage({ kind: "config.update", version: 1, config: makeMachineConfig() });
      worker.postMessage(makeInit(segments));

      await readyPromise;

      // Ensure the status flag was set (READY + shared status should be consistent).
      expect(Atomics.load(views.status, StatusIndex.IoReady)).toBe(1);

      // Ensure we did not start the IO IPC server: an AIPC command should not be serviced.
      // Push the command first, then wait (also used for the guest-RAM sentinel check) before
      // asserting the event ring is still empty, so we don't rely on tight timing.
      const cmdRing = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
      const evtRing = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);
      // Defensive: drain any junk.
      while (evtRing.tryPop()) {
        // ignore
      }
      expect(
        cmdRing.tryPush(
          encodeCommand({
            kind: "portRead",
            id: 1,
            port: 0x0060, // i8042 data port in legacy mode
            size: 1,
          }),
        ),
      ).toBe(true);

      // Wait a beat to catch any accidental background/tick writes (and any accidental AIPC server response).
      await new Promise<void>((resolve) => setTimeout(resolve, 50));
      expect(evtRing.tryPop()).toBe(null);
      expect(arraysEqual(views.guestU8, guestCopy)).toBe(true);

      const pausedPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "vm.snapshot.paused" && (msg as { requestId?: unknown }).requestId === 1,
        2000,
      ) as Promise<{ kind: string; requestId: number; ok: boolean }>;
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      const paused = await pausedPromise;
      expect(paused.ok).toBe(true);

      const resumedPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "vm.snapshot.resumed" && (msg as { requestId?: unknown }).requestId === 2,
        2000,
      ) as Promise<{ kind: string; requestId: number; ok: boolean }>;
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });
      const resumed = await resumedPromise;
      expect(resumed.ok).toBe(true);

      // Confirm the pause/resume handling didn't accidentally touch guest RAM.
      await new Promise<void>((resolve) => setTimeout(resolve, 50));
      expect(arraysEqual(views.guestU8, guestCopy)).toBe(true);
    } finally {
      await worker.terminate();
    }
  }, 20000);
});
