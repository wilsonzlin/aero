import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { createSharedMemoryViews, StatusIndex, type SharedMemorySegments } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { emptySetBootDisksMessage, type SetBootDisksMessage } from "../runtime/boot_disks_protocol";

async function waitForWorkerMessage(worker: Worker, predicate: (msg: unknown) => boolean, timeoutMs: number): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const effectiveTimeoutMs = timeoutMs * 2;
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${effectiveTimeoutMs}ms waiting for worker message`));
    }, effectiveTimeoutMs);
    (timer as unknown as { unref?: () => void }).unref?.();

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
    vgaFramebuffer: segments.sharedFramebuffer,
    scanoutState: segments.scanoutState,
    scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
    cursorState: segments.cursorState,
    cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
    ioIpcSab: segments.ioIpc,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
    // Mirror the coordinator's shared-memory preference: shared guest RAM implies threaded build.
    wasmVariant: "threaded",
  };
}

describe("workers/io.worker (worker_threads)", () => {
  it("runs as a host-only stub in vmRuntime=machine mode (READY without setBootDisks; ignores boot disk opens)", async () => {
    const segments = allocateHarnessSharedMemorySegments({
      guestRamBytes: 64 * 1024,
      sharedFramebuffer: new SharedArrayBuffer(8),
      sharedFramebufferOffsetBytes: 0,
      ioIpcBytes: 0,
      vramBytes: 0,
    });
    const views = createSharedMemoryViews(segments);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const webworkerShimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const diskSpyShimUrl = new URL("./test_workers/io_worker_disk_client_spy_shim.ts", import.meta.url);

    const worker = new Worker(new URL("./io.worker.ts", import.meta.url), {
      type: "module",
      execArgv: [
        "--experimental-strip-types",
        "--import",
        registerUrl.href,
        "--import",
        webworkerShimUrl.href,
        "--import",
        diskSpyShimUrl.href,
      ],
    } as unknown as WorkerOptions);

    try {
      const readyPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "io",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig({ vmRuntime: "machine" }),
      });

      // Do NOT send setBootDisks: machine host-only mode should still reach READY.
      worker.postMessage(makeInit(segments));
      await readyPromise;
      expect(Atomics.load(views.status, StatusIndex.IoReady)).toBe(1);

      const diskWorkerCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "test.worker.created", 500);
      worker.postMessage({
        ...emptySetBootDisksMessage(),
        mounts: {},
        // Provide a non-null stub object so legacy code paths would attempt to open.
        hdd: {
          source: "local",
          id: "dummy",
          name: "dummy",
          backend: "opfs",
          kind: "hdd",
          format: "raw",
          fileName: "dummy.img",
          sizeBytes: 0,
          createdAtMs: 0,
        },
        cd: null,
      } satisfies SetBootDisksMessage);

      await expect(diskWorkerCreated).rejects.toThrow(/timed out/i);
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});
