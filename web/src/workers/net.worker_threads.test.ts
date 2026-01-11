import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { encodeCommand } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { decodeL2Message, encodeL2Frame, L2_TUNNEL_TYPE_FRAME } from "../shared/l2TunnelProtocol";
import {
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  allocateSharedMemorySegments,
  ringRegionsForWorker,
  type SharedMemorySegments,
} from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";

async function waitForWorkerMessage(worker: Worker, predicate: (msg: unknown) => boolean, timeoutMs: number): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${timeoutMs}ms waiting for worker message`));
    }, timeoutMs);
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

function makeConfig(proxyUrl: string | null): AeroConfig {
  return {
    guestMemoryMiB: 1,
    enableWorkers: true,
    enableWebGPU: false,
    proxyUrl,
    activeDiskImage: null,
    logLevel: "info",
  };
}

function makeInit(segments: SharedMemorySegments): WorkerInitMessage {
  return {
    kind: "init",
    role: "net",
    controlSab: segments.control,
    guestMemory: segments.guestMemory,
    vgaFramebuffer: segments.vgaFramebuffer,
    ioIpcSab: segments.ioIpc,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
  };
}

describe("workers/net.worker (worker_threads)", () => {
  it("forwards NET_TX frames over the L2 tunnel and delivers inbound frames to NET_RX", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });

    const netTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
    const netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
    const commandRing = new RingBuffer(segments.control, ringRegionsForWorker("net").command.byteOffset);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      // Configure the worker to connect to an L2 tunnel.
      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("https://gateway.example.com") });
      worker.postMessage(makeInit(segments));

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/l2");

      await workerReady;

      const frame = Uint8Array.of(1, 2, 3, 4, 5);
      while (!netTxRing.tryPush(frame)) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }

      const wsSent = (await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.sent", 10000)) as {
        data?: Uint8Array;
      };
      expect(wsSent.data).toBeInstanceOf(Uint8Array);
      const wire = wsSent.data!;

      const decoded = decodeL2Message(wire);
      expect(decoded.type).toBe(L2_TUNNEL_TYPE_FRAME);
      expect(Array.from(decoded.payload)).toEqual(Array.from(frame));

      const inbound = Uint8Array.of(9, 8, 7);
      worker.postMessage({ type: "ws.inject", data: encodeL2Frame(inbound) });

      const rxDeadline = Date.now() + 2000;
      let received: Uint8Array | null = null;
      while (!received && Date.now() < rxDeadline) {
        received = netRxRing.tryPop();
        if (!received) {
          await new Promise<void>((resolve) => setTimeout(resolve, 5));
        }
      }

      expect(received).not.toBeNull();
      expect(Array.from(received!)).toEqual(Array.from(inbound));

      // Ensure pending RX frames flush promptly once the guest consumes NET_RX.
      // Fill NET_RX to capacity with a small number of large records, then inject another frame.
      const fillerLen = 64 * 1024;
      const filler = new Uint8Array(fillerLen);
      filler.fill(0xaa);
      let fillerCount = 0;
      while (netRxRing.tryPush(filler)) fillerCount += 1;
      expect(fillerCount).toBeGreaterThan(0);

      const inbound2 = Uint8Array.of(4, 3, 2, 1);
      worker.postMessage({ type: "ws.inject", data: encodeL2Frame(inbound2) });

      // Give the worker a chance to observe pendingRx>0 and park on the NET_RX head.
      await new Promise<void>((resolve) => setTimeout(resolve, 300));

      const flushStart = Date.now();
      const flushDeadline = flushStart + 2000;
      let flushed: Uint8Array | null = null;
      while (!flushed && Date.now() < flushDeadline) {
        const didConsume = netRxRing.consumeNext((payload) => {
          if (
            payload.byteLength === inbound2.byteLength &&
            payload[0] === inbound2[0] &&
            payload[1] === inbound2[1] &&
            payload[2] === inbound2[2] &&
            payload[3] === inbound2[3]
          ) {
            flushed = payload.slice();
          }
        });
        if (!didConsume) {
          await new Promise<void>((resolve) => setTimeout(resolve, 5));
        } else if (!flushed) {
          // Give the worker a chance to observe the freed space and flush pending RX.
          await new Promise<void>((resolve) => setTimeout(resolve, 0));
        }
      }
      expect(flushed).not.toBeNull();
      expect(Array.from(flushed!)).toEqual(Array.from(inbound2));
      expect(Date.now() - flushStart).toBeLessThan(500);

      // If the tunnel closes unexpectedly, the net worker should reconnect and
      // resume forwarding frames.
      worker.postMessage({ type: "ws.close", code: 1000, reason: "test" });
      const wsCreated2 = (await waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "ws.created",
        10000,
      )) as { url?: string };
      expect(wsCreated2.url).toBe("wss://gateway.example.com/l2");

      const frame2 = Uint8Array.of(6, 7, 8);
      while (!netTxRing.tryPush(frame2)) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }

      const wsSent2 = (await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.sent", 10000)) as {
        data?: Uint8Array;
      };
      expect(wsSent2.data).toBeInstanceOf(Uint8Array);
      const decoded2 = decodeL2Message(wsSent2.data!);
      expect(decoded2.type).toBe(L2_TUNNEL_TYPE_FRAME);
      expect(Array.from(decoded2.payload)).toEqual(Array.from(frame2));
    } finally {
      await worker.terminate();
    }
  }, 20000);

  it("wakes promptly on shutdown commands even while pending RX frames are buffered", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: 1 });

    const netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
    const commandRing = new RingBuffer(segments.control, ringRegionsForWorker("net").command.byteOffset);

    // Ensure the worker takes the `Atomics.waitAsync` scheduling path (otherwise it
    // already polls in short slices and this test is less meaningful).
    if (typeof (Atomics as any).waitAsync !== "function") return;

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000);
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("https://gateway.example.com") });
      worker.postMessage(makeInit(segments));

      await wsCreated;
      await workerReady;

      // Fill NET_RX to force inbound frames into the forwarder's pending queue.
      const fillerLen = 64 * 1024;
      const filler = new Uint8Array(fillerLen);
      filler.fill(0xaa);
      let fillerCount = 0;
      while (netRxRing.tryPush(filler)) fillerCount += 1;
      expect(fillerCount).toBeGreaterThan(0);

      // Inject a frame that cannot be delivered immediately due to NET_RX being full.
      worker.postMessage({ type: "ws.inject", data: encodeL2Frame(Uint8Array.of(1, 2, 3, 4)) });

      // Allow the worker to observe pendingRx>0 and park in the 1s pending-RX wait.
      await new Promise<void>((resolve) => setTimeout(resolve, 300));

      const shutdownStart = Date.now();
      expect(commandRing.tryPush(encodeCommand({ kind: "shutdown" }))).toBe(true);

      // With the command ring included in the Promise.race, the worker should wake
      // quickly (without waiting for the 1s pending-RX timeout).
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => {
          cleanup();
          reject(new Error("timed out waiting for net worker to exit after shutdown command"));
        }, 800);

        const onExit = () => {
          cleanup();
          resolve();
        };

        const onError = (err: unknown) => {
          cleanup();
          reject(err instanceof Error ? err : new Error(String(err)));
        };

        function cleanup(): void {
          clearTimeout(timer);
          worker.off("exit", onExit);
          worker.off("error", onError);
        }

        worker.on("exit", onExit);
        worker.on("error", onError);
      });

      expect(Date.now() - shutdownStart).toBeLessThan(500);
    } finally {
      await worker.terminate();
    }
  }, 20000);
});
