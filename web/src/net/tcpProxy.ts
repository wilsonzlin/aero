import { WebSocketTcpMuxProxyClient, type TcpMuxProxyOptions } from "./tcpMuxProxy.ts";
import { buildWebSocketUrl } from "./wsUrl.ts";

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

  constructor(proxyBaseUrl: string, sink: TcpProxyEventSink) {
    this.proxyBaseUrl = proxyBaseUrl;
    this.sink = sink;
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
        this.sink({
          type: "data",
          connectionId,
          data: new Uint8Array(evt.data),
        });
      }
    };
    ws.onerror = (err) => this.sink({ type: "error", connectionId, error: err });
    ws.onclose = () => this.sink({ type: "closed", connectionId });

    this.sockets.set(connectionId, ws);
  }

  send(connectionId: number, data: Uint8Array): void {
    const ws = this.sockets.get(connectionId);
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(data);
  }

  close(connectionId: number): void {
    const ws = this.sockets.get(connectionId);
    if (!ws) return;
    this.sockets.delete(connectionId);
    ws.close();
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
