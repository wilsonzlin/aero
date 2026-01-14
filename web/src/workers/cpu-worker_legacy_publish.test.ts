import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import { allocateSharedMemorySegments, ringRegionsForWorker } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";
import { encodeCommand } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { SharedFramebufferHeaderIndex, SHARED_FRAMEBUFFER_HEADER_U32_LEN } from "../ipc/shared-layout";

async function waitForWorkerMessage(
  worker: Worker,
  predicate: (msg: unknown) => boolean,
  timeoutMs: number,
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${timeoutMs}ms waiting for worker message`));
    }, timeoutMs);
    (timer as unknown as { unref?: () => void }).unref?.();

    const onMessage = (msg: unknown) => {
      // Surface runtime worker errors eagerly.
      const maybeProtocol = msg as Partial<ProtocolMessage> | undefined;
      if (maybeProtocol?.type === MessageType.ERROR) {
        cleanup();
        const errMsg = typeof (maybeProtocol as { message?: unknown }).message === "string" ? (maybeProtocol as any).message : "";
        reject(new Error(`worker reported error${errMsg ? `: ${errMsg}` : ""}`));
        return;
      }
      try {
        if (!predicate(msg)) return;
      } catch (err) {
        cleanup();
        reject(err instanceof Error ? err : new Error(String(err)));
        return;
      }
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

describe("workers/cpu.worker legacy framebuffer publishing", () => {
  it("publishes demo frames into sharedFramebuffer (no vgaFramebuffer segment)", async () => {
    // Use a tiny guest RAM size; this forces the shared framebuffer to be a standalone
    // SharedArrayBuffer, which exercises the CPU worker's JS publish path (no in-wasm demo).
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1, vramMiB: 0 });

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/worker_threads_webworker_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./cpu.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "cpu",
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        scanoutState: segments.scanoutState,
        scanoutStateOffsetBytes: segments.scanoutStateOffsetBytes,
        cursorState: segments.cursorState,
        cursorStateOffsetBytes: segments.cursorStateOffsetBytes,
      };

      worker.postMessage(initMsg);
      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "cpu",
        20_000,
      );

      const regions = ringRegionsForWorker("cpu");
      const commandRing = new RingBuffer(segments.control, regions.command.byteOffset);

      // Kick the worker's demo loop (equivalent to what the coordinator does on READY).
      expect(commandRing.tryPush(encodeCommand({ kind: "nop", seq: 1 }))).toBe(true);

      // Wait for at least one frame publish into the shared-layout header.
      const header = new Int32Array(
        segments.sharedFramebuffer,
        segments.sharedFramebufferOffsetBytes,
        SHARED_FRAMEBUFFER_HEADER_U32_LEN,
      );
      const seq0 = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
      const waitResult = Atomics.wait(header, SharedFramebufferHeaderIndex.FRAME_SEQ, seq0, 5_000);
      expect(waitResult === "ok" || waitResult === "not-equal").toBe(true);
      const seq1 = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
      expect(seq1).toBeGreaterThan(seq0);
    } finally {
      await worker.terminate();
    }
  }, 30_000);
});

