import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { encodeCommand } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import {
  decodeL2Message,
  encodeL2Frame,
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX,
  L2_TUNNEL_TYPE_FRAME,
} from "../shared/l2TunnelProtocol";
import {
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  createIoIpcSab,
  ringRegionsForWorker,
  type SharedMemorySegments,
} from "../runtime/shared_layout";
import { allocateHarnessSharedMemorySegments } from "../runtime/harness_shared_memory";
import { MessageType, type ProtocolMessage, type WorkerInitMessage } from "../runtime/protocol";

function arraysEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  for (let i = 0; i < a.byteLength; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function parsePcapng(bytes: Uint8Array): {
  interfaces: Array<{ name: string | null; linkType: number }>;
  packets: Array<{ payload: Uint8Array; interfaceId: number; epbFlags: number | null }>;
} {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const interfaces: Array<{ name: string | null; linkType: number }> = [];
  const packets: Array<{ payload: Uint8Array; interfaceId: number; epbFlags: number | null }> = [];
  const textDecoder = new TextDecoder();

  let off = 0;
  while (off + 12 <= bytes.byteLength) {
    const blockType = view.getUint32(off, true);
    const blockLen = view.getUint32(off + 4, true);
    if (blockLen < 12) throw new Error(`pcapng: invalid block length ${blockLen} at ${off}`);
    if (off + blockLen > bytes.byteLength) throw new Error(`pcapng: block overruns buffer at ${off} (len=${blockLen})`);
    const trailer = view.getUint32(off + blockLen - 4, true);
    if (trailer !== blockLen) throw new Error(`pcapng: mismatched block trailer at ${off} (${trailer} != ${blockLen})`);

    const bodyStart = off + 8;
    const bodyEnd = off + blockLen - 4;

    // Interface Description Block.
    if (blockType === 0x00000001) {
      // IDB fixed body is 8 bytes: linktype(u16), reserved(u16), snaplen(u32).
      const linkType = view.getUint16(bodyStart, true);
      let optOff = bodyStart + 8;
      let name: string | null = null;
      while (optOff + 4 <= bodyEnd) {
        const code = view.getUint16(optOff, true);
        const len = view.getUint16(optOff + 2, true);
        const valueStart = optOff + 4;
        const valueEnd = valueStart + len;
        if (valueEnd > bodyEnd) throw new Error(`pcapng: IDB option overruns block at ${off}`);
        if (code === 0) break;
        if (code === 2) {
          name = textDecoder.decode(bytes.subarray(valueStart, valueEnd));
        }
        optOff = valueStart + ((len + 3) & ~3);
      }
      interfaces.push({ name, linkType });
      off += blockLen;
      continue;
    }

    // Enhanced Packet Block.
    if (blockType === 0x00000006) {
      const interfaceId = view.getUint32(bodyStart, true);
      const capturedLen = view.getUint32(bodyStart + 12, true);
      const packetDataStart = bodyStart + 20;
      const packetDataEnd = packetDataStart + capturedLen;
      if (packetDataEnd > bodyEnd) throw new Error(`pcapng: EPB packet data overruns block at ${off}`);
      const payload = bytes.subarray(packetDataStart, packetDataEnd).slice();

      let epbFlags: number | null = null;
      // Options begin after packet data, padded to 32-bit.
      let optOff = packetDataStart + ((capturedLen + 3) & ~3);
      while (optOff + 4 <= bodyEnd) {
        const code = view.getUint16(optOff, true);
        const len = view.getUint16(optOff + 2, true);
        const valueStart = optOff + 4;
        const valueEnd = valueStart + len;
        if (valueEnd > bodyEnd) throw new Error(`pcapng: EPB option overruns block at ${off}`);
        if (code === 0) break;
        if (code === 2 && len === 4) {
          epbFlags = view.getUint32(valueStart, true);
        }
        optOff = valueStart + ((len + 3) & ~3);
      }

      packets.push({ payload, interfaceId, epbFlags });
      off += blockLen;
      continue;
    }

    off += blockLen;
  }

  return { interfaces, packets };
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

function makeConfig(proxyUrl: string | null, extra: Partial<AeroConfig> = {}): AeroConfig {
  return {
    guestMemoryMiB: 1,
    vramMiB: 16,
    enableWorkers: true,
    enableWebGPU: false,
    activeDiskImage: null,
    logLevel: "info",
    ...extra,
    proxyUrl,
  };
}

const TEST_VRAM_MIB = 1;

function makeInit(segments: SharedMemorySegments): WorkerInitMessage {
  return {
    kind: "init",
    role: "net",
    controlSab: segments.control,
    guestMemory: segments.guestMemory,
    ioIpcSab: segments.ioIpc,
    sharedFramebuffer: segments.sharedFramebuffer,
    sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
  };
}

function allocateTestSegments(): SharedMemorySegments {
  return allocateHarnessSharedMemorySegments({
    guestRamBytes: 64 * 1024,
    sharedFramebuffer: new SharedArrayBuffer(8),
    sharedFramebufferOffsetBytes: 0,
    ioIpc: createIoIpcSab({ includeHidIn: false }),
    // Net worker doesn't use VRAM today, but keep a small segment allocated to match the
    // previous test setup (and to catch accidental dependencies).
    vramBytes: TEST_VRAM_MIB * 1024 * 1024,
  });
}

describe("workers/net.worker (worker_threads)", () => {
  it("supports L2 token auth via Sec-WebSocket-Protocol (subprotocol)", async () => {
    const segments = allocateTestSegments();
    const token = "sekrit";

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
        protocols?: unknown;
      }>;
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig("https://gateway.example.com", {
          l2TunnelToken: token,
          l2TunnelTokenTransport: "subprotocol",
        }),
      });
      worker.postMessage(makeInit(segments));

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/l2");
      expect(createdMsg.protocols).toEqual([L2_TUNNEL_SUBPROTOCOL, `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}${token}`]);

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 20000);

  it("supports L2 token auth via query param (tokenTransport=query)", async () => {
    const segments = allocateTestSegments();
    const token = "sekrit";

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
        protocols?: unknown;
      }>;
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      worker.postMessage({
        kind: "config.update",
        version: 1,
        config: makeConfig("https://gateway.example.com", {
          l2TunnelToken: token,
          l2TunnelTokenTransport: "query",
        }),
      });
      worker.postMessage(makeInit(segments));

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe(`wss://gateway.example.com/l2?token=${token}`);
      expect(createdMsg.protocols).toEqual([L2_TUNNEL_SUBPROTOCOL]);

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 20000);

  it("falls back to direct proxyUrl when POST /session is unavailable", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Simulate a direct L2 endpoint URL (already ends with /l2); the shim's
      // fetch() returns 404 for /session, so the worker must fall back to the
      // original proxyUrl.
      worker.postMessage({ type: "fetch.mode", mode: "404" });

      const fetchCalled = waitForWorkerMessage(
        worker,
        (msg) => (msg as { type?: unknown }).type === "fetch.called",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("wss://gateway.example.com/l2") });
      worker.postMessage(makeInit(segments));

      const fetchMsg = await fetchCalled;
      expect(fetchMsg.url).toBe("https://gateway.example.com/session");
      const fetchInit = fetchMsg.init as { method?: unknown; credentials?: unknown } | undefined;
      expect(fetchInit?.method).toBe("POST");
      expect(fetchInit?.credentials).toBe("include");

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/l2");

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 40000);

  it("falls back to /l2 derived from base proxyUrl when POST /session is unavailable", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      worker.postMessage({ type: "fetch.mode", mode: "404" });

      const fetchCalled = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "fetch.called" &&
          (msg as { url?: unknown }).url === "https://gateway.example.com/base/session",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("https://gateway.example.com/base") });
      worker.postMessage(makeInit(segments));

      const first = await Promise.race([
        fetchCalled.then((msg) => ({ kind: "fetch" as const, msg })),
        wsCreated.then((msg) => ({ kind: "ws" as const, msg })),
      ]);
      expect(first.kind).toBe("fetch");

      const fetchMsg = first.msg as { url?: string; init?: unknown };
      expect(fetchMsg.url).toBe("https://gateway.example.com/base/session");
      const fetchInit = fetchMsg.init as { method?: unknown; credentials?: unknown } | undefined;
      expect(fetchInit?.method).toBe("POST");
      expect(fetchInit?.credentials).toBe("include");

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/base/l2");

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 40000);

  it("falls back to connecting directly when POST /session throws (network error)", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      worker.postMessage({ type: "fetch.mode", mode: "throw" });

      const fetchCalled = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "fetch.called" &&
          (msg as { url?: unknown }).url === "https://gateway.example.com/base/session",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      // Use an explicit legacy `/eth` endpoint so the fallback URL is
      // distinguishable from the session-derived `/l2` endpoint.
      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("/base/eth") });
      worker.postMessage(makeInit(segments));

      const first = await Promise.race([
        fetchCalled.then((msg) => ({ kind: "fetch" as const, msg })),
        wsCreated.then((msg) => ({ kind: "ws" as const, msg })),
      ]);
      expect(first.kind).toBe("fetch");

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/base/eth");

      await workerReady;
    } finally {
      await worker.terminate();
    }
  }, 40000);

  it("forwards NET_TX frames over the L2 tunnel and delivers inbound frames to NET_RX", async () => {
    const segments = allocateTestSegments();

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
      const fetchCalledAbs = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "fetch.called" &&
          (msg as { url?: unknown }).url === "https://gateway.example.com/session",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
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

      const firstConnectFirst = await Promise.race([
        fetchCalledAbs.then((msg) => ({ kind: "fetch" as const, msg })),
        wsCreated.then((msg) => ({ kind: "ws" as const, msg })),
      ]);
      expect(firstConnectFirst.kind).toBe("fetch");

      const fetchAbsMsg = firstConnectFirst.msg as { url?: string; init?: unknown };
      expect(fetchAbsMsg.url).toBe("https://gateway.example.com/session");
      const fetchAbsInit = fetchAbsMsg.init as { method?: unknown; credentials?: unknown } | undefined;
      expect(fetchAbsInit?.method).toBe("POST");
      expect(fetchAbsInit?.credentials).toBe("include");

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/l2");

      await workerReady;

      // Switch to a same-origin relative path and ensure it resolves against the
      // worker's location (Node shim provides a stable https://gateway.example.com base).
      const fetchCalledRel = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "fetch.called" &&
          (msg as { url?: unknown }).url === "https://gateway.example.com/base/session",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
      const wsCreatedRel = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;
      worker.postMessage({ kind: "config.update", version: 2, config: makeConfig("/base") });

      const secondConnectFirst = await Promise.race([
        fetchCalledRel.then((msg) => ({ kind: "fetch" as const, msg })),
        wsCreatedRel.then((msg) => ({ kind: "ws" as const, msg })),
      ]);
      expect(secondConnectFirst.kind).toBe("fetch");

      const fetchRelMsg = secondConnectFirst.msg as { url?: string; init?: unknown };
      expect(fetchRelMsg.url).toBe("https://gateway.example.com/base/session");
      const fetchRelInit = fetchRelMsg.init as { method?: unknown; credentials?: unknown } | undefined;
      expect(fetchRelInit?.method).toBe("POST");
      expect(fetchRelInit?.credentials).toBe("include");

      const createdRelMsg = await wsCreatedRel;
      expect(createdRelMsg.url).toBe("wss://gateway.example.com/base/l2");

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
      const fetchCalledReconnect = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "fetch.called" &&
          (msg as { url?: unknown }).url === "https://gateway.example.com/base/session",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
      const wsCreated2Promise = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;

      worker.postMessage({ type: "ws.close", code: 1000, reason: "test" });

      const reconnectFirst = await Promise.race([
        fetchCalledReconnect.then((msg) => ({ kind: "fetch" as const, msg })),
        wsCreated2Promise.then((msg) => ({ kind: "ws" as const, msg })),
      ]);
      expect(reconnectFirst.kind).toBe("fetch");

      const fetchReconnectMsg = reconnectFirst.msg as { url?: string; init?: unknown };
      expect(fetchReconnectMsg.url).toBe("https://gateway.example.com/base/session");
      const fetchReconnectInit = fetchReconnectMsg.init as { method?: unknown; credentials?: unknown } | undefined;
      expect(fetchReconnectInit?.method).toBe("POST");
      expect(fetchReconnectInit?.credentials).toBe("include");

      const wsCreated2 = await wsCreated2Promise;
      expect(wsCreated2.url).toBe("wss://gateway.example.com/base/l2");

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
  }, 40000);

  it("does not forward NET_TX frames queued while the tunnel is closed after reconnect", async () => {
    const segments = allocateTestSegments();

    const netTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    type SentFrame = { seq: number; payload: Uint8Array };
    const sentFrames: SentFrame[] = [];
    const onWorkerMessage = (msg: unknown) => {
      const data = msg as { type?: unknown; data?: unknown; seq?: unknown };
      if (data.type !== "ws.sent") return;
      if (typeof data.seq !== "number") return;
      if (!(data.data instanceof Uint8Array)) return;
      try {
        const decoded = decodeL2Message(data.data);
        if (decoded.type !== L2_TUNNEL_TYPE_FRAME) return;
        // Copy to avoid aliasing the transport buffer.
        sentFrames.push({ seq: data.seq, payload: decoded.payload.slice() });
      } catch {
        // Ignore malformed payloads.
      }
    };
    worker.on("message", onWorkerMessage);

    try {
      const wsCreated1Promise = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        seq?: number;
      }>;
      const workerReady = waitForWorkerMessage(
        worker,
        (msg) => (msg as Partial<ProtocolMessage>)?.type === MessageType.READY && (msg as { role?: unknown }).role === "net",
        10000,
      );

      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("https://gateway.example.com") });
      worker.postMessage(makeInit(segments));

      const wsCreated1 = await wsCreated1Promise;
      await workerReady;
      const firstCreatedSeq = typeof wsCreated1.seq === "number" ? wsCreated1.seq : 0;

      const wsClosed = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.closed", 10000);
      const wsCreated2Promise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "ws.created" &&
          typeof (msg as { seq?: unknown }).seq === "number" &&
          (msg as { seq: number }).seq > firstCreatedSeq,
        10000,
      ) as Promise<{ seq?: number }>;

      // Force a disconnect.
      worker.postMessage({ type: "ws.close", code: 1000, reason: "test" });
      await wsClosed;

      // Push a batch of frames while the tunnel is down. The filler frames ensure
      // that, in the buggy implementation, at least some disconnect-window frames
      // survive until reconnect and would be forwarded as stale traffic.
      const filler = Uint8Array.of(0xfa, 0xce, 0x00, 0x01);
      const frameA = Uint8Array.of(0x41, 0x00, 0x00, 0x01);
      const frameB = Uint8Array.of(0x42, 0x00, 0x00, 0x02);
      const fillerCount = 128;
      for (let i = 0; i < fillerCount; i += 1) {
        while (!netTxRing.tryPush(filler)) {
          await new Promise<void>((resolve) => setTimeout(resolve, 0));
        }
      }
      while (!netTxRing.tryPush(frameA)) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }
      while (!netTxRing.tryPush(frameB)) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }

      const wsCreated2 = await wsCreated2Promise;
      const reconnectSeq = typeof wsCreated2.seq === "number" ? wsCreated2.seq : firstCreatedSeq;

      const frameC = Uint8Array.of(0x43, 0x00, 0x00, 0x03);
      const wsSentC = waitForWorkerMessage(
        worker,
        (msg) => {
          const sent = msg as { type?: unknown; data?: unknown; seq?: unknown };
          if (sent.type !== "ws.sent") return false;
          if (typeof sent.seq !== "number" || sent.seq <= reconnectSeq) return false;
          if (!(sent.data instanceof Uint8Array)) return false;
          try {
            const decoded = decodeL2Message(sent.data);
            return decoded.type === L2_TUNNEL_TYPE_FRAME && arraysEqual(decoded.payload, frameC);
          } catch {
            return false;
          }
        },
        10000,
      );

      // Frame pushed after reconnect should be forwarded.
      while (!netTxRing.tryPush(frameC)) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }
      await wsSentC;

      // Give the worker a moment to flush any additional queued sends so that we
      // can assert over all observed post-reconnect traffic without relying on
      // exact message counts.
      await new Promise<void>((resolve) => setTimeout(resolve, 100));

      const postReconnect = sentFrames.filter((f) => f.seq > reconnectSeq).map((f) => f.payload);
      expect(postReconnect.some((payload) => arraysEqual(payload, frameC))).toBe(true);
      expect(postReconnect.some((payload) => arraysEqual(payload, frameA))).toBe(false);
      expect(postReconnect.some((payload) => arraysEqual(payload, frameB))).toBe(false);
    } finally {
      worker.off("message", onWorkerMessage);
      await worker.terminate();
    }
  }, 20000);

  it("falls back to connecting directly when POST /session fails", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Force the node shim `fetch()` to return a non-2xx response so the worker
      // takes the fallback path.
      worker.postMessage({ type: "fetch.mode", mode: "404" });

      const fetchCalled = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "fetch.called" &&
          (msg as { url?: unknown }).url === "https://gateway.example.com/base/session",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;

      // Use an explicit legacy `/eth` endpoint so the fallback URL is
      // distinguishable from the session-derived `/l2` endpoint.
      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("/base/eth") });
      worker.postMessage(makeInit(segments));

      const first = await Promise.race([
        fetchCalled.then((msg) => ({ kind: "fetch" as const, msg })),
        wsCreated.then((msg) => ({ kind: "ws" as const, msg })),
      ]);
      expect(first.kind).toBe("fetch");

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/base/eth");
    } finally {
      await worker.terminate();
    }
  }, 40000);

  it("falls back to connecting directly when POST /session returns invalid JSON", async () => {
    const segments = allocateTestSegments();

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
    const shimUrl = new URL("./test_workers/net_worker_node_shim.ts", import.meta.url);
    const worker = new Worker(new URL("./net.worker.ts", import.meta.url), {
      type: "module",
      execArgv: ["--experimental-strip-types", "--import", registerUrl.href, "--import", shimUrl.href],
    } as unknown as WorkerOptions);

    try {
      // Force the node shim `fetch()` to return a 2xx response body that is not
      // valid JSON so the worker takes the fallback path.
      worker.postMessage({ type: "fetch.mode", mode: "bad_json" });

      const fetchCalled = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { type?: unknown }).type === "fetch.called" &&
          (msg as { url?: unknown }).url === "https://gateway.example.com/base/session",
        10000,
      ) as Promise<{ url?: string; init?: unknown }>;
      const wsCreated = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000) as Promise<{
        url?: string;
      }>;

      // Use an explicit legacy `/eth` endpoint so the fallback URL is
      // distinguishable from the session-derived `/l2` endpoint.
      worker.postMessage({ kind: "config.update", version: 1, config: makeConfig("/base/eth") });
      worker.postMessage(makeInit(segments));

      const first = await Promise.race([
        fetchCalled.then((msg) => ({ kind: "fetch" as const, msg })),
        wsCreated.then((msg) => ({ kind: "ws" as const, msg })),
      ]);
      expect(first.kind).toBe("fetch");

      const createdMsg = await wsCreated;
      expect(createdMsg.url).toBe("wss://gateway.example.com/base/eth");
    } finally {
      await worker.terminate();
    }
  }, 40000);

  it("captures guest_tx + guest_rx frames into a PCAPNG when tracing is enabled", async () => {
    const segments = allocateTestSegments();

    const netTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
    const netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);

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

      worker.postMessage({ kind: "net.trace.clear" });
      worker.postMessage({ kind: "net.trace.enable" });

      // Ensure the enable message has been processed before sending frames, to avoid
      // racing the worker's ring-buffer drain loop (which could otherwise forward
      // frames before tracing is enabled).
      const statusReadyPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "net.trace.status" && (msg as { requestId?: unknown }).requestId === 99,
        10000,
      ) as Promise<{ enabled?: boolean; records?: number }>;
      worker.postMessage({ kind: "net.trace.status", requestId: 99 });
      const statusReady = await statusReadyPromise;
      expect(statusReady.enabled).toBe(true);
      expect(statusReady.records).toBe(0);

      const txFrame = Uint8Array.of(0xde, 0xad, 0xbe, 0xef);
      while (!netTxRing.tryPush(txFrame)) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }
      // Wait until the frame is observed by the worker.
      await waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.sent", 10000);

      const rxFrame = Uint8Array.of(0x11, 0x22, 0x33, 0x44, 0x55);
      worker.postMessage({ type: "ws.inject", data: encodeL2Frame(rxFrame) });

      const rxDeadline = Date.now() + 2000;
      let received: Uint8Array | null = null;
      while (!received && Date.now() < rxDeadline) {
        received = netRxRing.tryPop();
        if (!received) {
          await new Promise<void>((resolve) => setTimeout(resolve, 5));
        }
      }
      expect(received).not.toBeNull();
      expect(arraysEqual(received!, rxFrame)).toBe(true);

      // `net.trace.export_pcapng` is a non-draining snapshot.
      const snapshotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "net.trace.pcapng" &&
          (msg as { requestId?: unknown }).requestId === 2,
        10000,
      ) as Promise<{ kind: string; requestId: number; bytes: ArrayBuffer }>;
      worker.postMessage({ kind: "net.trace.export_pcapng", requestId: 2 });
      const snapshotMsg = await snapshotPromise;
      expect(snapshotMsg.bytes).toBeInstanceOf(ArrayBuffer);

      const snapshotParsed = parsePcapng(new Uint8Array(snapshotMsg.bytes));
      const snapshotTx = snapshotParsed.packets.find((p) => arraysEqual(p.payload, txFrame));
      const snapshotRx = snapshotParsed.packets.find((p) => arraysEqual(p.payload, rxFrame));
      expect(snapshotTx).toBeTruthy();
      expect(snapshotRx).toBeTruthy();

      const statusAfterSnapshotPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "net.trace.status" &&
          (msg as { requestId?: unknown }).requestId === 101,
        10000,
      ) as Promise<{ enabled?: boolean; records?: number; bytes?: number }>;
      worker.postMessage({ kind: "net.trace.status", requestId: 101 });
      const statusAfterSnapshot = await statusAfterSnapshotPromise;
      expect(statusAfterSnapshot.enabled).toBe(true);
      expect(statusAfterSnapshot.records).toBe(2);
      expect(statusAfterSnapshot.bytes).toBe(txFrame.byteLength + rxFrame.byteLength);

      // Disabling tracing should prevent subsequent frames from being captured.
      worker.postMessage({ kind: "net.trace.disable" });
      const statusDisabledPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "net.trace.status" &&
          (msg as { requestId?: unknown }).requestId === 98,
        10000,
      ) as Promise<{ enabled?: boolean; records?: number }>;
      worker.postMessage({ kind: "net.trace.status", requestId: 98 });
      const statusDisabled = await statusDisabledPromise;
      expect(statusDisabled.enabled).toBe(false);
      expect(statusDisabled.records).toBe(2);

      const txFrame2 = Uint8Array.of(0xaa, 0xbb, 0xcc, 0xdd);
      while (!netTxRing.tryPush(txFrame2)) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      }
      await waitForWorkerMessage(worker, (msg) => {
        const sent = msg as { type?: unknown; data?: unknown };
        if (sent.type !== "ws.sent") return false;
        if (!(sent.data instanceof Uint8Array)) return false;
        try {
          const decoded = decodeL2Message(sent.data);
          return decoded.type === L2_TUNNEL_TYPE_FRAME && arraysEqual(decoded.payload, txFrame2);
        } catch {
          return false;
        }
      }, 10000);

      // Re-enable tracing so later assertions (and the UI behavior) remain consistent.
      worker.postMessage({ kind: "net.trace.enable" });
      const statusReenabledPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "net.trace.status" &&
          (msg as { requestId?: unknown }).requestId === 97,
        10000,
      ) as Promise<{ enabled?: boolean; records?: number }>;
      worker.postMessage({ kind: "net.trace.status", requestId: 97 });
      const statusReenabled = await statusReenabledPromise;
      expect(statusReenabled.enabled).toBe(true);
      expect(statusReenabled.records).toBe(2);

      const pcapngPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown; requestId?: unknown }).kind === "net.trace.pcapng" && (msg as { requestId?: unknown }).requestId === 1,
        10000,
      ) as Promise<{ kind: string; requestId: number; bytes: ArrayBuffer }>;
      worker.postMessage({ kind: "net.trace.take_pcapng", requestId: 1 });

      const pcapngMsg = await pcapngPromise;
      expect(pcapngMsg.bytes).toBeInstanceOf(ArrayBuffer);

      const parsed = parsePcapng(new Uint8Array(pcapngMsg.bytes));
      const guestEthId = parsed.interfaces.findIndex((iface) => iface.name === "guest-eth0");
      expect(guestEthId).toBe(0);
      expect(parsed.interfaces[guestEthId]?.linkType).toBe(1); // LINKTYPE_ETHERNET

      const tx = parsed.packets.find((p) => arraysEqual(p.payload, txFrame));
      const rx = parsed.packets.find((p) => arraysEqual(p.payload, rxFrame));
      const tx2 = parsed.packets.find((p) => arraysEqual(p.payload, txFrame2));

      expect(tx).toBeTruthy();
      expect(rx).toBeTruthy();
      expect(tx2).toBeFalsy();

      // Direction is encoded via `epb_flags` on a single Ethernet interface.
      expect(tx!.interfaceId).toBe(guestEthId);
      expect(rx!.interfaceId).toBe(guestEthId);

      // Also ensure `epb_flags` direction bits are set:
      // - 1 = inbound
      // - 2 = outbound
      expect((tx!.epbFlags ?? 0) & 0x3).toBe(2);
      expect((rx!.epbFlags ?? 0) & 0x3).toBe(1);

      // `net.trace.take_pcapng` drains the capture so subsequent stats report 0 records/bytes.
      const statusAfterPromise = waitForWorkerMessage(
        worker,
        (msg) =>
          (msg as { kind?: unknown; requestId?: unknown }).kind === "net.trace.status" &&
          (msg as { requestId?: unknown }).requestId === 100,
        10000,
      ) as Promise<{ enabled?: boolean; records?: number; bytes?: number }>;
      worker.postMessage({ kind: "net.trace.status", requestId: 100 });
      const statusAfter = await statusAfterPromise;
      expect(statusAfter.enabled).toBe(true);
      expect(statusAfter.records).toBe(0);
      expect(statusAfter.bytes).toBe(0);
    } finally {
      await worker.terminate();
    }
  }, 40000);

  it("pauses forwarding during VM snapshots and drains NET_TX/NET_RX without errors", async () => {
    const segments = allocateTestSegments();

    const netTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
    const netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);

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

      // Fill NET_RX to force an inbound frame into the forwarder's pending buffer.
      const fillerLen = 64 * 1024;
      const filler = new Uint8Array(fillerLen);
      filler.fill(0xaa);
      let fillerCount = 0;
      while (netRxRing.tryPush(filler)) fillerCount += 1;
      expect(fillerCount).toBeGreaterThan(0);

      const pendingInbound = Uint8Array.of(1, 2, 3, 4);
      worker.postMessage({ type: "ws.inject", data: encodeL2Frame(pendingInbound) });

      // Allow the worker to observe NET_RX full and buffer the inbound frame.
      await new Promise<void>((resolve) => setTimeout(resolve, 100));

      const pausedPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "vm.snapshot.paused",
        5000,
      ) as Promise<{ kind: string; requestId: number; ok: boolean }>;
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 1 });
      const paused = await pausedPromise;
      expect(paused.ok).toBe(true);

      // NET_RX should be drained (filler frames dropped) by the net worker.
      expect(netRxRing.tryPop()).toBeNull();

      // While paused, guest TX should not be forwarded to the tunnel.
      expect(netTxRing.tryPush(Uint8Array.of(9, 9, 9))).toBe(true);
      await expect(waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.sent", 200)).rejects.toThrow(/timed out/i);

      const wsCreatedAfterResume = waitForWorkerMessage(worker, (msg) => (msg as { type?: unknown }).type === "ws.created", 10000);
      const resumedPromise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "vm.snapshot.resumed",
        5000,
      ) as Promise<{ kind: string; requestId: number; ok: boolean }>;
      worker.postMessage({ kind: "vm.snapshot.resume", requestId: 2 });
      const resumed = await resumedPromise;
      expect(resumed.ok).toBe(true);
      await wsCreatedAfterResume;

      // Ensure the pending inbound frame from before pause was cleared (it should not
      // appear after resume once NET_RX has space).
      expect(netRxRing.tryPop()).toBeNull();

      // Confirm that normal delivery resumes for new inbound frames.
      const inbound2 = Uint8Array.of(7, 6, 5);
      worker.postMessage({ type: "ws.inject", data: encodeL2Frame(inbound2) });

      const rxDeadline = Date.now() + 2000;
      let received: Uint8Array | null = null;
      while (!received && Date.now() < rxDeadline) {
        received = netRxRing.tryPop();
        if (!received) {
          await new Promise<void>((resolve) => setTimeout(resolve, 5));
        }
      }
      expect(received).not.toBeNull();
      expect(Array.from(received!)).toEqual(Array.from(inbound2));

      // Pausing again with empty rings should succeed (drain loops should not throw).
      const paused2Promise = waitForWorkerMessage(
        worker,
        (msg) => (msg as { kind?: unknown }).kind === "vm.snapshot.paused" && (msg as { requestId?: unknown }).requestId === 3,
        5000,
      ) as Promise<{ kind: string; requestId: number; ok: boolean }>;
      worker.postMessage({ kind: "vm.snapshot.pause", requestId: 3 });
      const paused2 = await paused2Promise;
      expect(paused2.ok).toBe(true);
    } finally {
      await worker.terminate();
    }
  }, 40000);

  it("wakes promptly on shutdown commands even while pending RX frames are buffered", async () => {
    const segments = allocateTestSegments();

    const netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
    const commandRing = new RingBuffer(segments.control, ringRegionsForWorker("net").command.byteOffset);

    // Ensure the worker takes the `Atomics.waitAsync` scheduling path (otherwise it
    // already polls in short slices and this test is less meaningful).
    if (typeof (Atomics as unknown as { waitAsync?: unknown }).waitAsync !== "function") return;

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
  }, 40000);
});
