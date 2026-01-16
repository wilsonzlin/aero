/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { WebSocketL2TunnelClient, type L2TunnelClientOptions, type L2TunnelTokenTransport } from "../net/l2Tunnel";
import { L2TunnelForwarder } from "../net/l2TunnelForwarder";
import { L2TunnelTelemetry } from "../net/l2TunnelTelemetry";
import { NetTracer } from "../net/net_tracer";
import { perf } from "../perf/perf";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { formatOneLineError } from "../text";
import { readTextResponseWithLimit } from "../storage/response_json";
import {
  serializeVmSnapshotError,
  type CoordinatorToWorkerSnapshotMessage,
  type VmSnapshotPausedMessage,
  type VmSnapshotResumedMessage,
} from "../runtime/snapshot_protocol";
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

// Gateway session responses should be small JSON payloads; cap size to avoid pathological
// allocations if the gateway is misconfigured or attacker-controlled.
const MAX_GATEWAY_SESSION_RESPONSE_BYTES = 1024 * 1024; // 1 MiB

const tracer = new NetTracer();
const traceFrame = (ev: { direction: "guest_tx" | "guest_rx"; frame: Uint8Array }): void => {
  try {
    tracer.recordEthernet(ev.direction, ev.frame);
  } catch {
    // Net tracing must never interfere with forwarding.
  }
};

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

let l2ReconnectAttempts = 0;
let l2ReconnectTimer: number | null = null;
let l2ReconnectGeneration = 0;

type GatewaySessionResponse = Readonly<{
  endpoints?: Readonly<{
    l2?: string;
  }>;
  limits?: Readonly<{
    l2?: Readonly<{
      maxFramePayloadBytes?: number;
      maxControlPayloadBytes?: number;
    }>;
  }>;
}>;

let l2BootstrapPromise: Promise<void> | null = null;
let l2BootstrapProxyUrl: string | null = null;
let l2BootstrapGeneration = 0;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

let snapshotPaused = false;
let snapshotResumePromise: Promise<void> | null = null;
let snapshotResumeResolve: (() => void) | null = null;

let shuttingDown = false;

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfLastFrameId = 0;
let perfIoMs = 0;
let perfIoReadBytes = 0;
let perfIoWriteBytes = 0;
let perfLastTxBytes = 0;
let perfLastRxBytes = 0;

// Even when idle, wake periodically so pending tunnel→guest frames buffered due
// to NET_RX backpressure get a chance to flush without waiting for NET_TX
// activity.
const NET_IDLE_WAIT_MS = 250;
const NET_PENDING_RX_POLL_MS = 20;
// Mirror the IO worker tick rate so we don't spin/drain NET_TX aggressively when
// the tunnel is backpressured (L2TunnelForwarder intentionally stops draining on
// send errors to avoid turning transient stalls into bursts of drops).
const NET_TX_BACKPRESSURE_SLEEP_MS = 8;
const L2_STATS_LOG_INTERVAL_MS = 1000;

const L2_RECONNECT_BASE_DELAY_MS = 250;
const L2_RECONNECT_MAX_DELAY_MS = 30_000;
const L2_RECONNECT_JITTER_FRACTION = 0.2;

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

function resolveUrlAgainstLocation(raw: string): URL {
  const baseHref = (globalThis as unknown as { location?: { href?: unknown } }).location?.href;
  return baseHref && typeof baseHref === "string" ? new URL(raw, baseHref) : new URL(raw);
}

function buildSessionUrl(proxyUrl: string): string {
  const url = resolveUrlAgainstLocation(proxyUrl);

  // `fetch()` does not support ws(s) schemes. If the caller passed a WebSocket
  // URL (explicit `/l2`), map it back to the HTTP origin for session bootstrap.
  if (url.protocol === "ws:") url.protocol = "http:";
  if (url.protocol === "wss:") url.protocol = "https:";

  // Session bootstrap is an HTTP endpoint and does not accept query parameters.
  url.search = "";
  url.hash = "";

  // `proxyUrl` may be a base path (append `/l2` later) or an explicit L2
  // endpoint (`.../l2` or legacy `.../eth`). The session endpoint is a sibling.
  let path = url.pathname.replace(/\/$/, "");
  if (path.endsWith("/l2") || path.endsWith("/eth")) {
    path = path.replace(/\/(l2|eth)$/, "");
  }

  url.pathname = `${path.replace(/\/$/, "")}/session`;
  return url.toString();
}

function buildWebSocketUrlFromEndpoint(proxyUrl: string, endpoint: string): string {
  const url = resolveUrlAgainstLocation(proxyUrl);
  if (url.protocol === "http:") url.protocol = "ws:";
  if (url.protocol === "https:") url.protocol = "wss:";
  url.search = "";
  url.hash = "";

  // `proxyUrl` may already include `/l2` or legacy `/eth`.
  let basePath = url.pathname.replace(/\/$/, "");
  if (basePath.endsWith("/l2") || basePath.endsWith("/eth")) {
    basePath = basePath.replace(/\/(l2|eth)$/, "");
  }

  // The gateway session response uses path-like strings (usually beginning with
  // `/`). Treat them as relative to the configured gateway base path so that
  // deployments behind a reverse-proxy prefix (`/base`) continue to work.
  const trimmedEndpoint = endpoint.trim();

  const normalizedBase = basePath.replace(/\/$/, "");
  if (
    normalizedBase.length > 0 &&
    trimmedEndpoint.startsWith("/") &&
    trimmedEndpoint.startsWith(`${normalizedBase}/`)
  ) {
    // If the endpoint already includes the base prefix, avoid duplicating it.
    url.pathname = trimmedEndpoint;
    return url.toString();
  }

  const endpointPath = trimmedEndpoint.startsWith("/") ? trimmedEndpoint.slice(1) : trimmedEndpoint;
  url.pathname = `${normalizedBase}/${endpointPath}`.replace(/^\/\//, "/");

  return url.toString();
}

function parseGatewaySessionResponse(text: string): GatewaySessionResponse {
  const trimmed = text.trim();
  if (trimmed.length === 0) return {};
  const json: unknown = JSON.parse(trimmed);
  if (typeof json !== "object" || json === null) return {};
  return json as GatewaySessionResponse;
}

function shouldIgnorePostMessageResponses(): boolean {
  if (shuttingDown) return true;
  try {
    return Atomics.load(status, StatusIndex.StopRequested) === 1;
  } catch {
    return false;
  }
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

function computeBackoffDelayMs(attempt: number, baseDelayMs: number, maxDelayMs: number, jitterFraction: number): number {
  // attempt is 1-based.
  const unclamped = baseDelayMs * 2 ** Math.max(0, attempt - 1);
  const delay = Math.min(maxDelayMs, unclamped);
  const jitter = delay * jitterFraction;
  const randomized = delay + (Math.random() * 2 - 1) * jitter;
  return Math.max(0, Math.round(randomized));
}

function clearReconnectTimer(): void {
  const timer = l2ReconnectTimer;
  if (timer === null) return;
  clearTimeout(timer);
  l2ReconnectTimer = null;
}

function scheduleReconnect(): void {
  if (snapshotPaused) return;
  if (l2TunnelProxyUrl === null) return;
  if (!l2Forwarder) return;
  if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;
  if (l2ReconnectTimer !== null) return;

  l2ReconnectAttempts += 1;
  const generation = ++l2ReconnectGeneration;
  const proxyUrl = l2TunnelProxyUrl;
  const delayMs = computeBackoffDelayMs(
    l2ReconnectAttempts,
    L2_RECONNECT_BASE_DELAY_MS,
    L2_RECONNECT_MAX_DELAY_MS,
    L2_RECONNECT_JITTER_FRACTION,
  );

  const timer = setTimeout(() => {
    l2ReconnectTimer = null;
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;
    if (snapshotPaused) return;
    if (generation !== l2ReconnectGeneration) return;
    if (l2TunnelProxyUrl !== proxyUrl) return;

    // The low-level WebSocketL2TunnelClient does not reconnect after close (it
    // becomes permanently closed), so create a fresh client.
    l2TunnelClient = null;
    try {
      applyL2TunnelConfig(currentConfig);
    } catch (err) {
      const message = formatOneLineError(err, 512);
      pushEvent({ kind: "log", level: "warn", message: `Failed to reconnect L2 tunnel: ${message}` });
    }
  }, delayMs);
  // Unit tests run this worker under node/worker_threads; unref the backoff timer
  // so a leaked net worker does not keep the test runner alive.
  (timer as unknown as { unref?: () => void }).unref?.();
  l2ReconnectTimer = timer as unknown as number;
}

async function connectL2TunnelWithBootstrap(proxyUrl: string, generation: number): Promise<void> {
  const forwarder = l2Forwarder;
  if (!forwarder) return;

  // Best-effort bootstrap: failures should not prevent connecting in dev setups
  // that do not require session auth.
  let session: GatewaySessionResponse | null = null;
  try {
    const res = await fetch(buildSessionUrl(proxyUrl), {
      method: "POST",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: "{}",
    });

    let text = "";
    try {
      text = await readTextResponseWithLimit(res, {
        maxBytes: MAX_GATEWAY_SESSION_RESPONSE_BYTES,
        label: "gateway session response",
      });
    } catch {
      text = "";
    }

    if (!res.ok) {
      // Do not reflect response bodies in log-visible errors.
      throw new Error(`failed to bootstrap gateway session (${res.status})`);
    }

    session = parseGatewaySessionResponse(text);
  } catch (err) {
    const message = formatOneLineError(err, 512);
    pushEvent({ kind: "log", level: "warn", message: `Failed to bootstrap gateway session: ${message}` });
  }

  // Avoid races with config updates / reconnect generations while the fetch is in-flight.
  if (generation !== l2ReconnectGeneration) return;
  if (l2TunnelProxyUrl !== proxyUrl) return;
  if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;

  // If another task already created a tunnel, do not clobber it.
  if (l2TunnelClient) return;

  const endpoint = session?.endpoints?.l2;
  let wsBaseUrl = proxyUrl;
  if (typeof endpoint === "string" && endpoint.trim().length > 0) {
    const trimmedEndpoint = endpoint.trim();
    try {
      // Absolute endpoint (includes scheme) should be honored as-is.
      wsBaseUrl = new URL(trimmedEndpoint).toString();
    } catch {
      // Relative endpoint: resolve against the configured gateway base.
      wsBaseUrl = buildWebSocketUrlFromEndpoint(proxyUrl, trimmedEndpoint);
    }
  }

  const maxFramePayloadBytes = session?.limits?.l2?.maxFramePayloadBytes;
  const maxControlPayloadBytes = session?.limits?.l2?.maxControlPayloadBytes;
  const tunnelOpts: L2TunnelClientOptions = {};
  if (typeof maxFramePayloadBytes === "number" && Number.isInteger(maxFramePayloadBytes) && maxFramePayloadBytes > 0) {
    tunnelOpts.maxFrameSize = maxFramePayloadBytes;
  }
  if (
    typeof maxControlPayloadBytes === "number" &&
    Number.isInteger(maxControlPayloadBytes) &&
    maxControlPayloadBytes > 0
  ) {
    tunnelOpts.maxControlSize = maxControlPayloadBytes;
  }

  const tokenRaw = currentConfig?.l2TunnelToken;
  const token = typeof tokenRaw === "string" ? tokenRaw.trim() : "";
  if (token) {
    tunnelOpts.token = token;
    const tokenTransportRaw = currentConfig?.l2TunnelTokenTransport;
    const tokenTransport: L2TunnelTokenTransport | null =
      tokenTransportRaw === "query" || tokenTransportRaw === "subprotocol" || tokenTransportRaw === "both" ? tokenTransportRaw : null;
    if (tokenTransport) {
      tunnelOpts.tokenTransport = tokenTransport;
    }
  }
  const tunnelOptions = Object.keys(tunnelOpts).length > 0 ? tunnelOpts : undefined;

  const client = new WebSocketL2TunnelClient(
    wsBaseUrl,
    (ev) => {
      // Avoid stale events from previously replaced tunnels clobbering telemetry state.
      if (l2TunnelClient !== client) return;
      forwarder.sink(ev);
    },
    tunnelOptions,
  );

  l2TunnelClient = client;
  forwarder.setTunnel(l2TunnelClient);

  if (l2TunnelTelemetry && l2TunnelTelemetry.connectionState !== "open") {
    l2TunnelTelemetry.onConnectInitiated();
  }

  forwarder.start();
}

function applyL2TunnelConfig(config: AeroConfig | null): void {
  const proxyUrl = config?.proxyUrl ?? null;
  const forwarder = l2Forwarder;
  const telemetry = l2TunnelTelemetry;
  if (!forwarder) return;

  // Snapshot orchestration can pause the net worker. While paused, keep the
  // forwarder stopped (no tunnel traffic) and defer reconnecting until resume.
  if (snapshotPaused) {
    clearReconnectTimer();
    // Ensure any existing tunnel is closed and transient buffers are cleared.
    telemetry?.onStopped();
    l2TunnelClient = null;
    forwarder.stop();
    return;
  }

  // Ensure we stop/close the previous tunnel when the proxy URL changes.
  if (proxyUrl !== l2TunnelProxyUrl) {
    clearReconnectTimer();
    l2ReconnectAttempts = 0;
    l2ReconnectGeneration += 1;

    telemetry?.onStopped();
    // Drop the reference early so any late events from the old tunnel are ignored.
    l2TunnelClient = null;
    forwarder.stop();
    l2TunnelProxyUrl = proxyUrl;
  }

  if (proxyUrl === null) {
    clearReconnectTimer();
    l2ReconnectAttempts = 0;
    l2ReconnectGeneration += 1;

    telemetry?.onStopped();
    return;
  }

  if (telemetry && telemetry.connectionState !== "open") {
    telemetry.onConnectInitiated();
  }

  if (!l2TunnelClient) {
    const generation = l2ReconnectGeneration;
    if (
      l2BootstrapPromise &&
      l2BootstrapProxyUrl === proxyUrl &&
      l2BootstrapGeneration === generation
    ) {
      return;
    }

    l2BootstrapProxyUrl = proxyUrl;
    l2BootstrapGeneration = generation;
    l2BootstrapPromise = connectL2TunnelWithBootstrap(proxyUrl, generation).finally(() => {
      if (l2BootstrapProxyUrl === proxyUrl && l2BootstrapGeneration === generation) {
        l2BootstrapPromise = null;
      }
    });
    return;
  }

  forwarder.start();
}

function drainRing(ring: RingBuffer | null): void {
  if (!ring) return;
  while (ring.consumeNext(() => {})) {
    // drop
  }
}

function handleSnapshotPause(): void {
  snapshotPaused = true;

  // Prevent any pending reconnect from running, and invalidate any in-flight backoff timer.
  clearReconnectTimer();
  l2ReconnectAttempts = 0;
  l2ReconnectGeneration += 1;

  // Ignore late events from the currently active tunnel while we close it.
  l2TunnelClient = null;
  l2TunnelTelemetry?.onStopped();
  l2Forwarder?.stop();

  // Clear transient guest<->host ring traffic so snapshot restore doesn't
  // observe stale frames.
  drainRing(netTxRing);
  drainRing(netRxRing);
}

function handleSnapshotResume(): void {
  snapshotPaused = false;
  // Wake the run loop immediately so we resume without waiting for command ring
  // or NET_TX activity.
  snapshotResumeResolve?.();
  snapshotResumePromise = null;
  snapshotResumeResolve = null;
  applyL2TunnelConfig(currentConfig);
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

function isPerfActive(): boolean {
  const header = perfFrameHeader;
  return !!perfWriter && !!header && Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
}

function maybeEmitPerfSample(): void {
  if (!perfWriter || !perfFrameHeader) return;
  const enabled = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
  const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
  if (!enabled) {
    perfLastFrameId = frameId;
    perfIoMs = 0;
    perfIoReadBytes = 0;
    perfIoWriteBytes = 0;
    return;
  }
  if (frameId === 0) {
    // Perf is enabled, but the main thread hasn't published a frame ID yet.
    // Keep accumulating so the first non-zero frame can include this interval.
    perfLastFrameId = 0;
    return;
  }
  if (perfLastFrameId === 0) {
    // First observed frame ID after enabling perf. Only emit if we have some
    // accumulated work; otherwise establish a baseline and wait for the next
    // frame boundary.
    if (perfIoMs <= 0 && perfIoReadBytes === 0 && perfIoWriteBytes === 0) {
      perfLastFrameId = frameId;
      return;
    }
  }
  if (frameId === perfLastFrameId) return;
  perfLastFrameId = frameId;

  const ioMs = perfIoMs > 0 ? perfIoMs : 0.01;
  perfWriter.frameSample(frameId, {
    durations: { io_ms: ioMs },
    counters: { io_read_bytes: perfIoReadBytes, io_write_bytes: perfIoWriteBytes },
  });
  perfIoMs = 0;
  perfIoReadBytes = 0;
  perfIoWriteBytes = 0;
}

async function runLoop(): Promise<void> {
  const forwarder = l2Forwarder;
  const txRing = netTxRing;
  const rxRing = netRxRing;
  const cmdRing = commandRing;
  if (!forwarder || !txRing || !rxRing) {
    // Some tiny test/demo coordinator configs omit the NET_TX/NET_RX AIPC rings to reduce
    // shared-memory allocations. In that mode the net worker remains idle but still responds
    // to runtime control commands (shutdown/snapshot orchestration).
    while (Atomics.load(status, StatusIndex.StopRequested) !== 1) {
      if (snapshotPaused) {
        drainRuntimeCommands();
        if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;
        if (!snapshotResumePromise) {
          snapshotResumePromise = new Promise<void>((resolve) => {
            snapshotResumeResolve = resolve;
          });
        }
        await Promise.race([snapshotResumePromise, cmdRing.waitForDataAsync(1000)]);
        continue;
      }

      drainRuntimeCommands();
      if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;
      await cmdRing.waitForDataAsync(1000);
    }

    pushEvent({ kind: "log", level: "info", message: "worker shutdown" });
    shuttingDown = true;
    l2TunnelClient = null;
    l2Forwarder?.stop();
    l2Forwarder = null;
    l2TunnelProxyUrl = null;
    l2TunnelTelemetry = null;
    setReadyFlag(status, role, false);
    ctx.close();
    return;
  }

  const hasWaitAsync = typeof (Atomics as unknown as { waitAsync?: unknown }).waitAsync === "function";

  // Use a single loop: drain control commands, pump the forwarder, then park on
  // the NET_TX ring while idle. The `waitForDataAsync` call keeps the worker
  // responsive to WebSocket and `postMessage()` events while avoiding spin.
  let lastTxBackpressureDrops = 0;
  while (Atomics.load(status, StatusIndex.StopRequested) !== 1) {
    if (snapshotPaused) {
      // VM execution is paused for snapshotting. Do not touch NET_TX/NET_RX or the
      // tunnel while paused; only drain runtime commands so shutdown can still be
      // processed.
      drainRuntimeCommands();
      if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

      if (!snapshotResumePromise) {
        snapshotResumePromise = new Promise<void>((resolve) => {
          snapshotResumeResolve = resolve;
        });
      }
      await Promise.race([snapshotResumePromise, cmdRing.waitForDataAsync(1000)]);
      continue;
    }

    const perfActive = isPerfActive();
    const t0 = perfActive ? nowMs() : 0;

    const now = nowMs();
    drainRuntimeCommands();
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    if (snapshotPaused) {
      // Snapshot pause: freeze NET_TX/NET_RX activity while the coordinator snapshots
      // shared state. Keep servicing runtime control commands (above) so shutdown can
      // still proceed.
      await cmdRing.waitForDataAsync(100);
      continue;
    }

    forwarder.tick();
    if (l2TunnelProxyUrl !== null) {
      l2TunnelTelemetry?.tick(now);
    }

    const stats = forwarder.stats();
    const pendingRx = stats.rxPendingFrames > 0;
    const txBackpressureDrops = stats.txDroppedTunnelBackpressure;
    const sawTxBackpressure = txBackpressureDrops !== lastTxBackpressureDrops;
    lastTxBackpressureDrops = txBackpressureDrops;

    const txDeltaBytes = stats.txBytes - perfLastTxBytes;
    const rxDeltaBytes = stats.rxBytes - perfLastRxBytes;
    perfLastTxBytes = stats.txBytes;
    perfLastRxBytes = stats.rxBytes;
    if (perfActive) {
      if (Number.isFinite(txDeltaBytes) && txDeltaBytes > 0) perfIoWriteBytes += txDeltaBytes;
      if (Number.isFinite(rxDeltaBytes) && rxDeltaBytes > 0) perfIoReadBytes += rxDeltaBytes;
    }

    if (perfActive) perfIoMs += nowMs() - t0;
    maybeEmitPerfSample();

    if (sawTxBackpressure) {
      await new Promise((resolve) => setTimeout(resolve, NET_TX_BACKPRESSURE_SLEEP_MS));
      continue;
    }
    const timeoutMs = pendingRx ? (hasWaitAsync ? L2_STATS_LOG_INTERVAL_MS : NET_PENDING_RX_POLL_MS) : NET_IDLE_WAIT_MS;
    if (hasWaitAsync) {
      if (pendingRx) {
        // Wake on:
        // - NET_TX data (guest wants to transmit)
        // - NET_RX consumption (guest freed space for pending RX flush)
        // - runtime control commands (e.g. shutdown)
        await Promise.race([
          txRing.waitForDataAsync(timeoutMs),
          rxRing.waitForConsumeAsync(timeoutMs),
          cmdRing.waitForDataAsync(timeoutMs),
        ]);
      } else {
        // When idle, also wake on runtime commands (shutdown) so the coordinator
        // doesn't wait for the NET idle timeout to elapse.
        await Promise.race([txRing.waitForDataAsync(timeoutMs), cmdRing.waitForDataAsync(timeoutMs)]);
      }
    } else {
      // In worker contexts without `Atomics.waitAsync`, `RingBuffer.waitForDataAsync()`
      // falls back to a tight polling loop. Use a short blocking `Atomics.wait()` slice
      // instead, then yield to service `postMessage` / WebSocket events.
      try {
        txRing.waitForData(Math.min(timeoutMs, NET_PENDING_RX_POLL_MS));
      } catch {
        await txRing.waitForDataAsync(timeoutMs);
        continue;
      }
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  }

  pushEvent({ kind: "log", level: "info", message: "worker shutdown" });
  shuttingDown = true;
  // Ignore any close/error events emitted as part of teardown.
  l2TunnelClient = null;
  l2Forwarder?.stop();
  l2Forwarder = null;
  l2TunnelProxyUrl = null;
  l2TunnelTelemetry = null;
  setReadyFlag(status, role, false);
  ctx.close();
}

function fatal(err: unknown): void {
  shuttingDown = true;
  // Ignore any close/error events emitted as part of teardown.
  l2TunnelClient = null;
  l2Forwarder?.stop();
  l2Forwarder = null;
  l2TunnelProxyUrl = null;
  l2TunnelTelemetry = null;

  const message = formatOneLineError(err, 512);
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
        scanoutState: init.scanoutState,
        scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
        cursorState: init.cursorState,
        cursorStateOffsetBytes: init.cursorStateOffsetBytes ?? 0,
        ioIpc: init.ioIpcSab!,
        sharedFramebuffer: init.sharedFramebuffer!,
        sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
      };
      status = createSharedMemoryViews(segments).status;

      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);

      try {
        netTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
        netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
      } catch {
        netTxRing = null;
        netRxRing = null;
      }

      if (netTxRing && netRxRing) {
        l2Forwarder = new L2TunnelForwarder(netTxRing, netRxRing, {
          onTunnelEvent: (ev) => {
            l2TunnelTelemetry?.onTunnelEvent(ev);
            if (ev.type === "open") {
              clearReconnectTimer();
              l2ReconnectAttempts = 0;
              return;
            }
            if (ev.type === "close") {
              // Drop the reference early so late events from the closed tunnel
              // don't race with a new client instance.
              l2TunnelClient = null;
              // Treat a closed tunnel as "no tunnel" for the forwarder so `sendFrame()`
              // failures don't behave like backpressure (which would otherwise stall
              // NET_TX draining and allow stale frames to leak after reconnect).
              l2Forwarder?.setTunnel(null);
              scheduleReconnect();
            }
          },
        });
        if (tracer.isEnabled()) {
          l2Forwarder.setOnFrame(traceFrame);
        }
        l2TunnelTelemetry = new L2TunnelTelemetry({
          intervalMs: L2_STATS_LOG_INTERVAL_MS,
          getStats: () => l2Forwarder!.stats(),
          emitLog: (level, message) => pushEvent({ kind: "log", level, message }),
        });
      } else {
        l2Forwarder = null;
        l2TunnelTelemetry = null;
        pushEvent({ kind: "log", level: "info", message: "NET_TX/NET_RX rings missing; network disabled." });
      }

      if (init.perfChannel) {
        perfWriter = new PerfWriter(init.perfChannel.buffer, {
          workerKind: init.perfChannel.workerKind,
          runStartEpochMs: init.perfChannel.runStartEpochMs,
        });
        perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
        perfLastFrameId = 0;
        perfIoMs = 0;
        perfIoReadBytes = 0;
        perfIoWriteBytes = 0;
        perfLastTxBytes = 0;
        perfLastRxBytes = 0;
      }

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
    const msg = ev.data as
      | Partial<WorkerInitMessage>
      | Partial<ConfigUpdateMessage>
      | Partial<CoordinatorToWorkerSnapshotMessage>
      | Partial<{ kind: "net.trace.enable" | "net.trace.disable" | "net.trace.clear" }>
      | Partial<{ kind: "net.trace.take_pcapng" | "net.trace.export_pcapng" | "net.trace.status"; requestId: number }>
      | undefined;
    if (!msg) return;

    if (msg.kind === "net.trace.enable") {
      tracer.enable();
      l2Forwarder?.setOnFrame(traceFrame);
      return;
    }

    if (msg.kind === "net.trace.disable") {
      tracer.disable();
      l2Forwarder?.setOnFrame(null);
      return;
    }

    if (msg.kind === "net.trace.clear") {
      tracer.clear();
      return;
    }

    if (msg.kind === "net.trace.status") {
      const requestId = (msg as { requestId?: unknown }).requestId;
      if (typeof requestId !== "number") return;
      if (shouldIgnorePostMessageResponses()) return;
      const stats = tracer.stats();
      if (shouldIgnorePostMessageResponses()) return;
      ctx.postMessage({ kind: "net.trace.status", requestId, ...stats });
      return;
    }

    if (msg.kind === "net.trace.take_pcapng") {
      const requestId = (msg as { requestId?: unknown }).requestId;
      if (typeof requestId !== "number") return;
      if (shouldIgnorePostMessageResponses()) return;
      const bytes = tracer.takePcapng();
      if (shouldIgnorePostMessageResponses()) return;
      // Ensure we transfer an ArrayBuffer (not a SharedArrayBuffer-backed view).
      const buf =
        bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength
          ? bytes.buffer
          : bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
      if (shouldIgnorePostMessageResponses()) return;
      ctx.postMessage({ kind: "net.trace.pcapng", requestId, bytes: buf }, [buf]);
      return;
    }

    if (msg.kind === "net.trace.export_pcapng") {
      const requestId = (msg as { requestId?: unknown }).requestId;
      if (typeof requestId !== "number") return;
      if (shouldIgnorePostMessageResponses()) return;
      const bytes = tracer.exportPcapng();
      if (shouldIgnorePostMessageResponses()) return;
      // Ensure we transfer an ArrayBuffer (not a SharedArrayBuffer-backed view).
      const buf =
        bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength
          ? bytes.buffer
          : bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
      if (shouldIgnorePostMessageResponses()) return;
      ctx.postMessage({ kind: "net.trace.pcapng", requestId, bytes: buf }, [buf]);
      return;
    }
    const snapshotMsg = msg as Partial<CoordinatorToWorkerSnapshotMessage>;
    if (typeof snapshotMsg.kind === "string" && snapshotMsg.kind.startsWith("vm.snapshot.")) {
      const requestId = snapshotMsg.requestId;
      if (typeof requestId !== "number") return;
      switch (snapshotMsg.kind) {
        case "vm.snapshot.pause": {
          try {
            handleSnapshotPause();
            ctx.postMessage({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
          } catch (err) {
            ctx.postMessage({
              kind: "vm.snapshot.paused",
              requestId,
              ok: false,
              error: serializeVmSnapshotError(err),
            } satisfies VmSnapshotPausedMessage);
          }
          return;
        }
        case "vm.snapshot.resume": {
          try {
            handleSnapshotResume();
            ctx.postMessage({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
          } catch (err) {
            ctx.postMessage({
              kind: "vm.snapshot.resumed",
              requestId,
              ok: false,
              error: serializeVmSnapshotError(err),
            } satisfies VmSnapshotResumedMessage);
          }
          return;
        }
        default:
          return;
      }
    }

    if (msg.kind === "config.update") {
      currentConfig = (msg as ConfigUpdateMessage).config;
      currentConfigVersion = (msg as ConfigUpdateMessage).version;
      ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);

      if (snapshotPaused) {
        // Defer applying config changes until snapshot resume to avoid reconnecting
        // and forwarding frames while paused.
        return;
      }

      try {
        applyL2TunnelConfig(currentConfig);
      } catch (err) {
        const message = formatOneLineError(err, 512);
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
