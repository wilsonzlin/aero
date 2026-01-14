import { describe, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
import { InputEventType } from "../input/event_queue";
import { allocateSharedMemorySegments, type SharedMemorySegments } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import type { SetBootDisksMessage } from "../runtime/boot_disks_protocol";

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
    (timer as unknown as { unref?: () => void }).unref?.();

    const onMessage = (msg: unknown) => {
      const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
      if (maybeProtocol?.type === MessageType.ERROR) {
        cleanup();
        const errMsg = typeof (maybeProtocol as { message?: unknown }).message === "string" ? (maybeProtocol as any).message : "";
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
    guestMemoryMiB: 1,
    enableWorkers: true,
    enableWebGPU: false,
    activeDiskImage: null,
    logLevel: "info",
    proxyUrl: null,
    ...extra,
  };
}

function makeInit(segments: SharedMemorySegments): WorkerInitMessage {
  return {
    kind: "init",
    role: "cpu",
    controlSab: segments.control,
    guestMemory: segments.guestMemory,
    vram: segments.vram,
    vramBasePaddr: segments.vram ? VRAM_BASE_PADDR : undefined,
    vramSizeBytes: segments.vram ? segments.vram.byteLength : undefined,
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

describe("workers/machine_cpu.worker (worker_threads)", () => {
  it("boots via config.update + init and reaches READY", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage({ type: "setBootDisks", mounts: {}, hdd: null, cd: null } satisfies SetBootDisksMessage);
      worker.postMessage(makeInit(segments));

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 20_000);

  it("recycles input batch buffers when requested (even without WASM)", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./machine_cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        10_000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig(),
      });
      worker.postMessage(makeInit(segments));
      await workerReady;

      const buf = new ArrayBuffer((2 + 4) * 4);
      const words = new Int32Array(buf);
      words[0] = 1; // count
      words[1] = 0; // timestamp (unused in this test)
      words[2] = InputEventType.KeyScancode;
      words[3] = 0; // event timestamp
      words[4] = 0x1c; // packed scancode bytes
      words[5] = 1; // len
      const expected = Array.from(words);
      const expectedByteLength = buf.byteLength;

      const recycledPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "in:input-batch-recycle",
        10_000,
      );

      worker.postMessage({ type: "in:input-batch", buffer: buf, recycle: true }, [buf]);

      const recycled = (await recycledPromise) as { buffer?: unknown };
      const recycledBuf = recycled.buffer;
      if (!(recycledBuf instanceof ArrayBuffer)) {
        throw new Error("expected in:input-batch-recycle to carry an ArrayBuffer");
      }
      if (recycledBuf.byteLength !== expectedByteLength) {
        throw new Error(`expected recycled buffer byteLength=${expectedByteLength}, got ${recycledBuf.byteLength}`);
      }
      const got = Array.from(new Int32Array(recycledBuf));
      if (got.join(",") !== expected.join(",")) {
        throw new Error(`unexpected recycled buffer contents: got [${got.join(",")}] expected [${expected.join(",")}]`);
      }
    } finally {
      await worker.terminate();
    }
  }, 20_000);
});
