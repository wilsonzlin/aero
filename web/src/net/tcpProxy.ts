import { WebSocketTcpMuxProxyClient, type TcpMuxProxyOptions } from "./tcpMuxProxy.ts";
import { buildWebSocketUrl } from "./wsUrl.ts";
import type { NetTracer } from "./net_tracer.ts";
import { wsCloseSafe, wsSendSafe } from "./wsSafe.ts";

export type TcpProxyEvent =
  | { type: "connected"; connectionId: number }
  | { type: "data"; connectionId: number; data: Uint8Array }
  | { type: "closed"; connectionId: number }
  | { type: "error"; connectionId: number; error: unknown };

export type TcpProxyEventSink = (event: TcpProxyEvent) => void;

/**
 * Browser TCP proxy client.
 *
 * The Rust stack emits a `TcpProxyConnect { connection_id, remote_ip, remote_port }` action, and
 * the host is responsible for opening a WebSocket to the proxy and forwarding data in both
 * directions.
 */
export class WebSocketTcpProxyClient {
  private readonly sockets = new Map<number, WebSocket>();
  private readonly proxyBaseUrl: string;
  private readonly sink: TcpProxyEventSink;
  private readonly tracer?: NetTracer;

  constructor(proxyBaseUrl: string, sink: TcpProxyEventSink, opts: { tracer?: NetTracer } = {}) {
    this.proxyBaseUrl = proxyBaseUrl;
    this.sink = sink;
    this.tracer = opts.tracer;
  }

  connect(connectionId: number, remoteIp: string, remotePort: number): void {
    if (this.sockets.has(connectionId)) return;

    const url = buildWebSocketUrl(this.proxyBaseUrl, "/tcp");
    url.searchParams.set("v", "1");
    // `remoteIp` may be an IPv6 literal. For the canonical host+port form we do
    // NOT require bracket syntax, but callers may already provide it.
    const host = remoteIp.startsWith("[") && remoteIp.endsWith("]") ? remoteIp.slice(1, -1) : remoteIp;
    url.searchParams.set("host", host);
    url.searchParams.set("port", String(remotePort));

    const ws = new WebSocket(url.toString());
    ws.binaryType = "arraybuffer";

    ws.onopen = () => this.sink({ type: "connected", connectionId });
    ws.onmessage = (evt) => {
      if (evt.data instanceof ArrayBuffer) {
        const data = new Uint8Array(evt.data);
        try {
          this.tracer?.recordTcpProxy("remote_to_guest", connectionId, data);
        } catch {
          // Best-effort tracing: never interfere with proxy traffic.
        }
        this.sink({
          type: "data",
          connectionId,
          data,
        });
      }
    };
    ws.onerror = (err) => this.sink({ type: "error", connectionId, error: err });
    ws.onclose = () => {
      // `connectionId` may be reused after a close; ensure we only remove the
      // current socket (and not a newer one that reconnected).
      if (this.sockets.get(connectionId) === ws) {
        this.sockets.delete(connectionId);
      }
      this.sink({ type: "closed", connectionId });
    };

    this.sockets.set(connectionId, ws);
  }

  send(connectionId: number, data: Uint8Array): void {
    const ws = this.sockets.get(connectionId);
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    try {
      this.tracer?.recordTcpProxy("guest_to_remote", connectionId, data);
    } catch {
      // Best-effort tracing: never interfere with proxy traffic.
    }
    if (!wsSendSafe(ws, data)) {
      wsCloseSafe(ws);
    }
  }

  close(connectionId: number): void {
    const ws = this.sockets.get(connectionId);
    if (!ws) return;
    this.sockets.delete(connectionId);
    wsCloseSafe(ws);
  }
}

/**
 * Multiplexed TCP proxy client that uses the gateway's `/tcp-mux` endpoint
 * (`aero-tcp-mux-v1` subprotocol) but exposes the same `TcpProxyEventSink`
 * interface as {@link WebSocketTcpProxyClient}.
 *
 * Use this when you need many concurrent TCP connections and want to avoid
 * per-WebSocket overhead and browser connection limits.
 */
export class WebSocketTcpProxyMuxClient {
  private readonly mux: WebSocketTcpMuxProxyClient;
  private readonly sink: TcpProxyEventSink;

  constructor(proxyBaseUrl: string, sink: TcpProxyEventSink, opts: TcpMuxProxyOptions = {}) {
    this.sink = sink;
    this.mux = new WebSocketTcpMuxProxyClient(proxyBaseUrl, opts);
    this.mux.onOpen = (streamId) => this.sink({ type: "connected", connectionId: streamId });
    this.mux.onData = (streamId, data) => this.sink({ type: "data", connectionId: streamId, data });
    this.mux.onClose = (streamId) => this.sink({ type: "closed", connectionId: streamId });
    this.mux.onError = (streamId, error) => this.sink({ type: "error", connectionId: streamId, error });
  }

  connect(connectionId: number, remoteIp: string, remotePort: number): void {
    // `remoteIp` may be an IPv6 literal. For the canonical host+port form we do
    // NOT require bracket syntax, but callers may already provide it.
    const host = remoteIp.startsWith("[") && remoteIp.endsWith("]") ? remoteIp.slice(1, -1) : remoteIp;
    this.mux.open(connectionId, host, remotePort);
  }

  send(connectionId: number, data: Uint8Array): void {
    this.mux.send(connectionId, data);
  }

  close(connectionId: number): void {
    this.mux.close(connectionId, { fin: true });
  }

  shutdown(): Promise<void> {
    return this.mux.shutdown();
  }
}
