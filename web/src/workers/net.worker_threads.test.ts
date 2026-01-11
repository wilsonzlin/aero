import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { decodeL2Message, encodeL2Frame, L2_TUNNEL_TYPE_FRAME } from "../shared/l2TunnelProtocol";
import {
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  allocateSharedMemorySegments,
  type SharedMemorySegments,
} from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";

async function waitForWorkerMessage(worker: Worker, predicate: (msg: unknown) => boolean, timeoutMs: number): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out after ${timeoutMs}ms waiting for worker message`));
    }, timeoutMs);

    const onMessage = (msg: unknown) => {
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

    const loaderUrl = new URL("../../../scripts/ts-transpile-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const loaderFlag = process.allowedNodeEnvironmentFlags.has("--loader") ? "--loader" : "--experimental-loader";
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: [loaderFlag, loaderUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Configure the worker to connect to an L2 tunnel.
      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("wss://gateway.example.com") });
      worker.postMessage(makeInit(segments));

      await waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

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
    } finally {
      await worker.terminate();
    }
  }, 20000);
});
