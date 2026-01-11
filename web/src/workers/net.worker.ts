/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { WebSocketL2TunnelClient } from "../net/l2Tunnel";
import { L2TunnelForwarder } from "../net/l2TunnelForwarder";
import { L2TunnelTelemetry } from "../net/l2TunnelTelemetry";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import {
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  StatusIndex,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  type WorkerRole,
} from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: WorkerRole = "net";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing: RingBuffer | null = null;

let netTxRing: RingBuffer | null = null;
let netRxRing: RingBuffer | null = null;

let l2Forwarder: L2TunnelForwarder | null = null;
let l2TunnelClient: WebSocketL2TunnelClient | null = null;
let l2TunnelProxyUrl: string | null = null;
let l2TunnelTelemetry: L2TunnelTelemetry | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

const NET_IDLE_WAIT_MS = 1000;
const NET_PENDING_RX_POLL_MS = 20;
const L2_STATS_LOG_INTERVAL_MS = 1000;

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

function pushEvent(evt: Event): void {
  if (!eventRing) return;
  eventRing.tryPush(encodeEvent(evt));
}

function pushEventBlocking(evt: Event, timeoutMs = 1000): void {
  if (!eventRing) return;
  const payload = encodeEvent(evt);
  if (eventRing.tryPush(payload)) return;
  try {
    eventRing.pushBlocking(payload, timeoutMs);
  } catch {
    // ignore
  }
}

function applyL2TunnelConfig(config: AeroConfig | null): void {
  const proxyUrl = config?.proxyUrl ?? null;
  const forwarder = l2Forwarder;
  const telemetry = l2TunnelTelemetry;
  if (!forwarder) return;

  // Ensure we stop/close the previous tunnel when the proxy URL changes.
  if (proxyUrl !== l2TunnelProxyUrl) {
    telemetry?.onStopped();
    forwarder.stop();
    l2TunnelClient = null;
    l2TunnelProxyUrl = proxyUrl;
  }

  if (proxyUrl === null) {
    telemetry?.onStopped();
    return;
  }

  if (!l2TunnelClient) {
    const client = new WebSocketL2TunnelClient(proxyUrl, (ev) => {
      // Avoid stale events from previously replaced tunnels clobbering telemetry state.
      if (l2TunnelClient !== client) return;
      forwarder.sink(ev);
    });
    l2TunnelClient = client;
    forwarder.setTunnel(l2TunnelClient);
  }

  if (telemetry && telemetry.connectionState !== "open") {
    telemetry.onConnectInitiated();
  }

  forwarder.start();
}

function drainRuntimeCommands(): void {
  while (true) {
    const bytes = commandRing.tryPop();
    if (!bytes) break;
    let cmd: Command;
    try {
      cmd = decodeCommand(bytes);
    } catch {
      continue;
    }
    if (cmd.kind === "shutdown") {
      Atomics.store(status, StatusIndex.StopRequested, 1);
    }
  }
}

async function runLoop(): Promise<void> {
  const forwarder = l2Forwarder;
  const txRing = netTxRing;
  if (!forwarder || !txRing) {
    throw new Error("net.worker was not initialized correctly (missing forwarder or rings).");
  }

  // Use a single loop: drain control commands, pump the forwarder, then park on
  // the NET_TX ring while idle. The `waitForDataAsync` call keeps the worker
  // responsive to WebSocket and `postMessage()` events while avoiding spin.
  while (Atomics.load(status, StatusIndex.StopRequested) !== 1) {
    drainRuntimeCommands();
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    forwarder.tick();

    if (l2TunnelProxyUrl !== null) {
      l2TunnelTelemetry?.tick(nowMs());
    }

    const pendingRx = forwarder.stats().rxPendingFrames > 0;
    const timeoutMs = pendingRx ? NET_PENDING_RX_POLL_MS : NET_IDLE_WAIT_MS;
    await txRing.waitForDataAsync(timeoutMs);
  }

  pushEvent({ kind: "log", level: "info", message: "worker shutdown" });
  l2Forwarder?.stop();
  l2Forwarder = null;
  l2TunnelClient = null;
  l2TunnelProxyUrl = null;
  l2TunnelTelemetry = null;
  setReadyFlag(status, role, false);
  ctx.close();
}

function fatal(err: unknown): void {
  l2Forwarder?.stop();
  l2Forwarder = null;
  l2TunnelClient = null;
  l2TunnelProxyUrl = null;
  l2TunnelTelemetry = null;

  const message = err instanceof Error ? err.message : String(err);
  pushEventBlocking({ kind: "panic", message });
  try {
    setReadyFlag(status, role, false);
  } catch {
    // ignore if we haven't initialized shared memory yet.
  }
  ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
  ctx.close();
}

async function initWorker(init: WorkerInitMessage): Promise<void> {
  perf.spanBegin("worker:boot");
  try {
    perf.spanBegin("worker:init");
    try {
      role = init.role ?? "net";
      const segments = {
        control: init.controlSab!,
        guestMemory: init.guestMemory!,
        vgaFramebuffer: init.vgaFramebuffer!,
        ioIpc: init.ioIpcSab!,
        sharedFramebuffer: init.sharedFramebuffer!,
        sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
      };
      status = createSharedMemoryViews(segments).status;

      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);

      netTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
      netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);

      l2Forwarder = new L2TunnelForwarder(netTxRing, netRxRing, { onTunnelEvent: (ev) => l2TunnelTelemetry?.onTunnelEvent(ev) });
      l2TunnelTelemetry = new L2TunnelTelemetry({
        intervalMs: L2_STATS_LOG_INTERVAL_MS,
        getStats: () => l2Forwarder!.stats(),
        emitLog: (level, message) => pushEvent({ kind: "log", level, message }),
      });

      // Apply any config already received before the init handshake completed.
      applyL2TunnelConfig(currentConfig);

      pushEvent({ kind: "log", level: "info", message: "worker ready" });
      setReadyFlag(status, role, true);
      ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
      if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role });
    } finally {
      perf.spanEnd("worker:init");
    }
  } finally {
    perf.spanEnd("worker:boot");
  }

  void runLoop().catch(fatal);
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  try {
    const msg = ev.data as Partial<WorkerInitMessage | ConfigUpdateMessage> | undefined;
    if (!msg) return;

    if (msg.kind === "config.update") {
      currentConfig = (msg as ConfigUpdateMessage).config;
      currentConfigVersion = (msg as ConfigUpdateMessage).version;
      ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);

      try {
        applyL2TunnelConfig(currentConfig);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        pushEvent({ kind: "log", level: "warn", message: `Failed to apply L2 tunnel config: ${message}` });
      }
      return;
    }

    if (msg.kind === "init") {
      void initWorker(msg as WorkerInitMessage).catch(fatal);
    }
  } catch (err) {
    fatal(err);
  }
};
