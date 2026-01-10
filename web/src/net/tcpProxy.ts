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

  constructor(
    private readonly proxyBaseUrl: string,
    private readonly sink: TcpProxyEventSink,
  ) {}

  connect(connectionId: number, remoteIp: string, remotePort: number): void {
    if (this.sockets.has(connectionId)) return;

    const url = new URL(this.proxyBaseUrl);
    url.pathname = `${url.pathname.replace(/\/$/, "")}/tcp`;
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
