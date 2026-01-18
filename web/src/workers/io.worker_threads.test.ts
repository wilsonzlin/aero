import { describe, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { unrefBestEffort } from "../unrefSafe";
import { VRAM_BASE_PADDR } from "../arch/guest_phys";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { type SharedMemorySegments } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { NET_WORKER_NODE_EXEC_ARGV } from "./test_utils/worker_exec_argv";

const WORKER_EXEC_ARGV = NET_WORKER_NODE_EXEC_ARGV;

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

function makeConfig(extra: Partial<AeroConfig> = {}): AeroConfig {
  return {
    ...extra,
    guestMemoryMiB: extra.guestMemoryMiB ?? 1,
    vramMiB: extra.vramMiB ?? 16,
    enableWorkers: extra.enableWorkers ?? true,
    enableWebGPU: extra.enableWebGPU ?? false,
    activeDiskImage: extra.activeDiskImage ?? null,
    logLevel: extra.logLevel ?? "info",
    proxyUrl: extra.proxyUrl ?? null,
    vmRuntime: extra.vmRuntime ?? "machine",
  };
}

function makeInit(segments: SharedMemorySegments): WorkerInitMessage {
  return {
    kind: "init",
    role: "io",
    controlSab: segments.control,
    guestMemory: segments.guestMemory,
    vram: segments.vram,
    vramBasePaddr: segments.vram ? VRAM_BASE_PADDR : undefined,
    vramSizeBytes: segments.vram ? segments.vram.byteLength : undefined,
    // Legacy VGA scanout uses the sharedFramebuffer region; newer runtimes may also
    // surface this via a dedicated field in `WorkerInitMessage`.
    vgaFramebuffer: segments.sharedFramebuffer,
    scanoutState: segments.scanoutState,
    scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    cursorState: segments.cursorState,
    cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
    ioIpcSab: segments.ioIpc,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
    wasmVariant: "threaded",
  };
}

describe("workers/io.worker (worker_threads)", () => {
  it("boots via config.update(vmRuntime=machine) + init and reaches READY without setBootDisks", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./io.worker.ts", import.meta.url), {
      type: "module",
      execArgv: WORKER_EXEC_ARGV,
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "io",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("ACKs vm.snapshot.pause/vm.snapshot.resume quickly in machine host-only mode", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });

    const worker = new Worker(new URL("./io.worker.ts", import.meta.url), {
      type: "module",
      execArgv: WORKER_EXEC_ARGV,
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "io",
        10_000,
      );

      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig() });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const pausedPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.paused" &&
          (msg as { requestId?: unknown }).requestId === 1 &&
          (msg as { ok?: unknown }).ok === true,
        5_000,
      );
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      await pausedPromise;

      const resumedPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown }).kind === "vm.snapshot.resumed" &&
          (msg as { requestId?: unknown }).requestId === 2 &&
          (msg as { ok?: unknown }).ok === true,
        5_000,
      );
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });
      await resumedPromise;
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});
