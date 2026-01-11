import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram, encodeUdpRelayV2Datagram } from "../shared/udpRelayProtocol";

export type UdpProxyEvent = {
  srcIp: string;
  srcPort: number;
  dstPort: number;
  data: Uint8Array;
};

export type UdpProxyEventSink = (event: UdpProxyEvent) => void;

const formatIpv4 = (octets: ArrayLike<number>) => `${octets[0]}.${octets[1]}.${octets[2]}.${octets[3]}`;

const formatIpv6 = (bytes: Uint8Array) => {
  // This intentionally does not perform :: zero-compression; it only needs to
  // produce a stable, valid string representation for logging/debugging.
  const parts: string[] = [];
  for (let i = 0; i < 16; i += 2) {
    const v = (bytes[i] << 8) | bytes[i + 1];
    parts.push(v.toString(16).padStart(4, "0"));
  }
  return parts.join(":");
};

function parseIpv4(ip: string): [number, number, number, number] {
  const parts = ip.split(".").map((s) => Number(s));
  if (parts.length !== 4 || parts.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) {
    throw new Error(`invalid IPv4 address: ${ip}`);
  }
  return [parts[0], parts[1], parts[2], parts[3]];
}

function parseIpv6(ip: string): Uint8Array {
  let raw = ip;
  if (raw.startsWith("[") && raw.endsWith("]")) {
    raw = raw.slice(1, -1);
  }
  const percent = raw.indexOf("%");
  if (percent !== -1) {
    raw = raw.slice(0, percent);
  }

  const expandGroups = (groups: string[]): number[] => {
    const out: number[] = [];
    for (let i = 0; i < groups.length; i++) {
      const g = groups[i];
      if (g === "") throw new Error(`invalid IPv6 address: ${ip}`);
      if (g.includes(".")) {
        if (i !== groups.length - 1) throw new Error(`invalid IPv6 address: ${ip}`);
        const ip4 = parseIpv4(g);
        out.push((ip4[0] << 8) | ip4[1], (ip4[2] << 8) | ip4[3]);
        continue;
      }
      const v = Number.parseInt(g, 16);
      if (!Number.isInteger(v) || v < 0 || v > 0xffff) {
        throw new Error(`invalid IPv6 address: ${ip}`);
      }
      out.push(v);
    }
    return out;
  };

  let groups: number[] = [];
  const doubleColon = raw.indexOf("::");
  if (doubleColon !== -1) {
    if (raw.indexOf("::", doubleColon + 2) !== -1) throw new Error(`invalid IPv6 address: ${ip}`);
    const head = raw.slice(0, doubleColon);
    const tail = raw.slice(doubleColon + 2);

    const headGroups = head === "" ? [] : head.split(":");
    const tailGroups = tail === "" ? [] : tail.split(":");

    const headVals = expandGroups(headGroups);
    const tailVals = expandGroups(tailGroups);
    const used = headVals.length + tailVals.length;
    if (used > 8) throw new Error(`invalid IPv6 address: ${ip}`);

    const missing = 8 - used;
    if (missing === 0) throw new Error(`invalid IPv6 address: ${ip}`);

    groups = [...headVals, ...new Array(missing).fill(0), ...tailVals];
  } else {
    const parts = raw.split(":");
    groups = expandGroups(parts);
    if (groups.length !== 8) throw new Error(`invalid IPv6 address: ${ip}`);
  }

  if (groups.length !== 8) throw new Error(`invalid IPv6 address: ${ip}`);

  const bytes = new Uint8Array(16);
  for (let i = 0; i < 8; i++) {
    const v = groups[i];
    bytes[i * 2] = (v >>> 8) & 0xff;
    bytes[i * 2 + 1] = v & 0xff;
  }
  return bytes;
}

function encodeDatagram(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): Uint8Array<ArrayBuffer> {
  if (dstIp.includes(":") || (dstIp.startsWith("[") && dstIp.endsWith("]"))) {
    const ip6 = parseIpv6(dstIp);
    return encodeUdpRelayV2Datagram({
      guestPort: srcPort,
      remoteIp: ip6,
      remotePort: dstPort,
      payload,
    }) as Uint8Array<ArrayBuffer>;
  }
  return encodeUdpRelayV1Datagram({
    guestPort: srcPort,
    remoteIpv4: parseIpv4(dstIp),
    remotePort: dstPort,
    payload,
  }) as Uint8Array<ArrayBuffer>;
}

/**
 * UDP proxy over WebSocket (fallback when WebRTC isn't available).
 *
 * Wire format is the same as the WebRTC UDP relay datagram framing. See:
 * - proxy/webrtc-udp-relay/PROTOCOL.md
 */
export type WebSocketUdpProxyAuth =
  | { apiKey: string; mode?: "first_message" | "query" }
  | { token: string; mode?: "first_message" | "query" };

export type WebSocketUdpProxyClientOptions = {
  /**
   * Optional auth for the /udp WebSocket endpoint.
   *
   * The relay supports:
   *  - query string auth (e.g. ?apiKey=... / ?token=...), and
   *  - first WebSocket message {type:"auth", ...} (preferred).
   */
  auth?: WebSocketUdpProxyAuth;

  /**
   * Maximum number of outbound datagrams to buffer while waiting for auth/ready.
   *
   * Defaults to a small bound to avoid unbounded memory growth if the server
   * never accepts the connection.
   */
  maxPendingDatagrams?: number;
};

export class WebSocketUdpProxyClient {
  private ws: WebSocket | null = null;
  private ready = false;
  private pending: Uint8Array[] = [];
  private readonly opts: WebSocketUdpProxyClientOptions;

  constructor(
    private readonly proxyBaseUrl: string,
    private readonly sink: UdpProxyEventSink,
    opts: WebSocketUdpProxyClientOptions = {},
  ) {
    this.opts = opts;
  }

  connect(): void {
    this.close();

    const url = new URL(this.proxyBaseUrl);
    if (url.protocol === "http:") url.protocol = "ws:";
    else if (url.protocol === "https:") url.protocol = "wss:";
    url.pathname = `${url.pathname.replace(/\/$/, "")}/udp`;

    const auth = this.opts.auth;
    const authMode = auth?.mode ?? "first_message";
    if (auth && authMode === "query") {
      if ("apiKey" in auth) url.searchParams.set("apiKey", auth.apiKey);
      else url.searchParams.set("token", auth.token);
    }

    const ws = new WebSocket(url.toString());
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      this.ready = false;
      this.pending = [];

      // If auth is configured, prefer sending it as the first WS message. We
      // wait for the relay's {"type":"ready"} acknowledgment before sending
      // any datagrams.
      if (auth && authMode === "first_message") {
        if ("apiKey" in auth) ws.send(JSON.stringify({ type: "auth", apiKey: auth.apiKey }));
        else ws.send(JSON.stringify({ type: "auth", token: auth.token }));
        return;
      }

      // Back-compat / auth-less mode: if no credentials are configured, allow
      // sending immediately (older dev relays may not send a ready message).
      if (!auth) {
        this.ready = true;
      }
    };
    ws.onmessage = (evt) => {
      if (typeof evt.data === "string") {
        // Control plane messages: {type:"ready"} / {type:"error", ...}
        try {
          const msg = JSON.parse(evt.data) as { type?: string };
          if (msg?.type === "ready") {
            this.ready = true;
            const queued = this.pending;
            this.pending = [];
            for (const pkt of queued) {
              if (ws.readyState !== WebSocket.OPEN) break;
              ws.send(pkt);
            }
          } else if (msg?.type === "error") {
            // Best-effort: close on structured error.
            this.close();
          }
        } catch {
          // Ignore malformed control messages.
        }
        return;
      }
      if (!(evt.data instanceof ArrayBuffer)) return;
      const buf = new Uint8Array(evt.data);
      try {
        const frame = decodeUdpRelayFrame(buf);
        if (frame.version === 1) {
          this.sink({
            srcIp: formatIpv4(frame.remoteIpv4),
            srcPort: frame.remotePort,
            dstPort: frame.guestPort,
            data: frame.payload,
          });
        } else {
          this.sink({
            srcIp: frame.addressFamily === 4 ? formatIpv4(frame.remoteIp) : formatIpv6(frame.remoteIp),
            srcPort: frame.remotePort,
            dstPort: frame.guestPort,
            data: frame.payload,
          });
        }
      } catch {
        // Drop malformed frames.
      }
    };
    ws.onclose = () => {
      this.ws = null;
      this.ready = false;
      this.pending = [];
    };
    this.ws = ws;
  }

  send(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    try {
      const pkt = encodeDatagram(srcPort, dstIp, dstPort, payload);
      if (this.ready) {
        this.ws.send(pkt);
        return;
      }

      // Auth is configured but not yet accepted; buffer a small amount to avoid
      // dropping early packets (e.g. DNS) during the handshake.
      const max = this.opts.maxPendingDatagrams ?? 128;
      if (this.pending.length < max) {
        this.pending.push(pkt);
      }
    } catch {
      // Drop invalid/oversized datagrams.
    }
  }

  close(): void {
    this.ws?.close();
    this.ws = null;
    this.ready = false;
    this.pending = [];
  }
}

/**
 * UDP proxy over WebRTC data channel.
 *
 * The signaling / ICE negotiation is intentionally left to the caller, since Aero's web app will
 * likely have an existing signaling channel for other purposes.
 */
export class WebRtcUdpProxyClient {
  constructor(
    private readonly channel: RTCDataChannel,
    private readonly sink: UdpProxyEventSink,
  ) {
    channel.binaryType = "arraybuffer";
    channel.onmessage = (evt) => {
      if (!(evt.data instanceof ArrayBuffer)) return;
      const buf = new Uint8Array(evt.data);
      try {
        const frame = decodeUdpRelayFrame(buf);
        if (frame.version === 1) {
          this.sink({
            srcIp: formatIpv4(frame.remoteIpv4),
            srcPort: frame.remotePort,
            dstPort: frame.guestPort,
            data: frame.payload,
          });
        } else {
          this.sink({
            srcIp: frame.addressFamily === 4 ? formatIpv4(frame.remoteIp) : formatIpv6(frame.remoteIp),
            srcPort: frame.remotePort,
            dstPort: frame.guestPort,
            data: frame.payload,
          });
        }
      } catch {
        // Drop malformed frames.
      }
    };
  }

  send(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): void {
    if (this.channel.readyState !== "open") return;
    try {
      this.channel.send(encodeDatagram(srcPort, dstIp, dstPort, payload));
    } catch {
      // Drop invalid/oversized datagrams.
    }
  }
}
