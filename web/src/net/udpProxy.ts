import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram, encodeUdpRelayV2Datagram } from "../shared/udpRelayProtocol";
import type { NetTracer } from "./net_tracer";
import { dcSendSafe } from "./rtcSafe";
import { buildWebSocketUrl } from "./wsUrl.ts";
import { unrefBestEffort } from "../unrefSafe";
import { wsCloseSafe, wsIsClosedSafe, wsIsOpenSafe, wsSendSafe } from "./wsSafe.ts";

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

  /**
   * How long to wait for the relay to send `{type:"ready"}` before rejecting
   * `connect()`.
   *
   * Defaults to 10s.
   */
  connectTimeoutMs?: number;

  /**
   * Optional network tracer hook (best-effort).
   */
  tracer?: NetTracer;
};

export class WebSocketUdpProxyClient {
  private ws: WebSocket | null = null;
  private ready = false;
  private pending: Uint8Array[] = [];
  private readonly proxyBaseUrl: string;
  private readonly sink: UdpProxyEventSink;
  private readonly opts: WebSocketUdpProxyClientOptions;
  private readonly tracer?: NetTracer;

  constructor(
    proxyBaseUrl: string,
    sink: UdpProxyEventSink,
    optsOrAuthToken: WebSocketUdpProxyClientOptions | string = {},
  ) {
    this.proxyBaseUrl = proxyBaseUrl;
    this.sink = sink;
    this.opts = typeof optsOrAuthToken === "string" ? { auth: { token: optsOrAuthToken } } : optsOrAuthToken;
    this.tracer = this.opts.tracer;
  }

  connect(): Promise<void> {
    this.close();

    const url = buildWebSocketUrl(this.proxyBaseUrl, "/udp");

    const auth = this.opts.auth;
    const authMode = auth?.mode ?? "first_message";
    const credential = auth ? ("apiKey" in auth ? auth.apiKey : auth.token) : null;
    if (credential && authMode === "query") {
      // Forward/compat: different relay builds may accept either token or apiKey
      // depending on auth mode. Supplying both allows the client to remain
      // agnostic.
      url.searchParams.set("token", credential);
      url.searchParams.set("apiKey", credential);
    }

    const ws = new WebSocket(url.toString());
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    this.ready = false;
    this.pending = [];

    const timeoutMs = this.opts.connectTimeoutMs ?? 10_000;

    return new Promise((resolve, reject) => {
      let settled = false;

      const settle = (err?: unknown) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        if (err) reject(err);
        else resolve();
      };

      const timer = setTimeout(() => {
        if (this.ws === ws) {
          wsCloseSafe(ws);
          this.ws = null;
          this.ready = false;
          this.pending = [];
        }
        settle(new Error("udp relay websocket timed out"));
      }, timeoutMs);
      unrefBestEffort(timer);

      ws.onopen = () => {
        if (this.ws !== ws) return;
        // If auth is configured, prefer sending it as the first WS message. We
        // wait for the relay's {"type":"ready"} acknowledgment before sending
        // any datagrams.
        if (credential && authMode === "first_message") {
          if (!wsSendSafe(ws, JSON.stringify({ type: "auth", token: credential, apiKey: credential }))) {
            settle(new Error("udp relay websocket send failed"));
            this.close();
          }
        }
      };

      ws.onmessage = (evt) => {
        if (this.ws !== ws) return;
        if (typeof evt.data === "string") {
          // Control plane messages: {type:"ready"} / {type:"error", ...}
          try {
            const msg = JSON.parse(evt.data) as { type?: string; code?: unknown; message?: unknown };
            if (msg?.type === "ready") {
              if (!this.ready) {
                this.ready = true;
                const queued = this.pending;
                this.pending = [];
                for (const pkt of queued) {
                  if (!wsSendSafe(ws, pkt)) break;
                }
              }
              settle();
            } else if (msg?.type === "error") {
              const code = typeof msg.code === "string" ? msg.code : "error";
              const message = typeof msg.message === "string" ? msg.message : "udp relay error";
              settle(new Error(`${code}: ${message}`));
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
            try {
              this.tracer?.recordUdpProxy(
                "remote_to_guest",
                "proxy",
                frame.remoteIpv4,
                frame.remotePort,
                frame.guestPort,
                frame.payload,
              );
            } catch {
              // Best-effort tracing: never interfere with proxy traffic.
            }
            this.sink({
              srcIp: formatIpv4(frame.remoteIpv4),
              srcPort: frame.remotePort,
              dstPort: frame.guestPort,
              data: frame.payload,
            });
          } else {
            // NetTracer's UDP pseudo-header currently only supports IPv4.
            // Skip tracing IPv6 datagrams until the format is extended.
            if (frame.addressFamily === 4) {
              const ip4: [number, number, number, number] = [frame.remoteIp[0]!, frame.remoteIp[1]!, frame.remoteIp[2]!, frame.remoteIp[3]!];
              try {
                this.tracer?.recordUdpProxy(
                  "remote_to_guest",
                  "proxy",
                  ip4,
                  frame.remotePort,
                  frame.guestPort,
                  frame.payload,
                );
              } catch {
                // Best-effort tracing: never interfere with proxy traffic.
              }
            }
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

      ws.onerror = () => {
        settle(new Error("udp relay websocket error"));
      };

      ws.onclose = (evt) => {
        if (this.ws === ws) {
          this.ws = null;
          this.ready = false;
          this.pending = [];
        }

        if (!settled) {
          const code = typeof evt?.code === "number" ? evt.code : 0;
          // Close reasons are server-controlled; do not reflect them in user-visible errors.
          settle(new Error(`udp relay websocket closed (${code})`));
        }
      };
    });
  }

  send(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): void {
    const ws = this.ws;
    if (!ws || wsIsClosedSafe(ws)) return;
    try {
      const pkt = encodeDatagram(srcPort, dstIp, dstPort, payload);
      if (this.ready && wsIsOpenSafe(ws)) {
        // NetTracer's UDP pseudo-header currently only supports IPv4.
        // Skip tracing IPv6 datagrams until the format is extended.
        if (!dstIp.includes(":") && !(dstIp.startsWith("[") && dstIp.endsWith("]"))) {
          try {
            this.tracer?.recordUdpProxy("guest_to_remote", "proxy", parseIpv4(dstIp), srcPort, dstPort, payload);
          } catch {
            // Best-effort tracing: never interfere with proxy traffic.
          }
        }
        if (!wsSendSafe(ws, pkt)) {
          this.close();
          return;
        }
        return;
      }

      // Auth is configured but not yet accepted; buffer a small amount to avoid
      // dropping early packets (e.g. DNS) during the handshake.
      const max = this.opts.maxPendingDatagrams ?? 128;
      if (this.pending.length < max) {
        // NetTracer's UDP pseudo-header currently only supports IPv4.
        // Skip tracing IPv6 datagrams until the format is extended.
        if (!dstIp.includes(":") && !(dstIp.startsWith("[") && dstIp.endsWith("]"))) {
          try {
            this.tracer?.recordUdpProxy("guest_to_remote", "proxy", parseIpv4(dstIp), srcPort, dstPort, payload);
          } catch {
            // Best-effort tracing: never interfere with proxy traffic.
          }
        }
        this.pending.push(pkt);
      }
    } catch {
      // Drop invalid/oversized datagrams.
    }
  }

  close(): void {
    if (this.ws) wsCloseSafe(this.ws);
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
  private readonly channel: RTCDataChannel;
  private readonly sink: UdpProxyEventSink;
  private readonly tracer?: NetTracer;

  constructor(
    channel: RTCDataChannel,
    sink: UdpProxyEventSink,
    opts: { tracer?: NetTracer } = {},
  ) {
    this.channel = channel;
    this.sink = sink;
    this.tracer = opts.tracer;
    channel.binaryType = "arraybuffer";
    channel.onmessage = (evt) => {
      if (!(evt.data instanceof ArrayBuffer)) return;
      const buf = new Uint8Array(evt.data);
      try {
        const frame = decodeUdpRelayFrame(buf);
        if (frame.version === 1) {
          try {
            this.tracer?.recordUdpProxy(
              "remote_to_guest",
              "webrtc",
              frame.remoteIpv4,
              frame.remotePort,
              frame.guestPort,
              frame.payload,
            );
          } catch {
            // Best-effort tracing: never interfere with proxy traffic.
          }
          this.sink({
            srcIp: formatIpv4(frame.remoteIpv4),
            srcPort: frame.remotePort,
            dstPort: frame.guestPort,
            data: frame.payload,
          });
        } else {
          // NetTracer's UDP pseudo-header currently only supports IPv4.
          // Skip tracing IPv6 datagrams until the format is extended.
          if (frame.addressFamily === 4) {
            const ip4: [number, number, number, number] = [frame.remoteIp[0]!, frame.remoteIp[1]!, frame.remoteIp[2]!, frame.remoteIp[3]!];
            try {
              this.tracer?.recordUdpProxy(
                "remote_to_guest",
                "webrtc",
                ip4,
                frame.remotePort,
                frame.guestPort,
                frame.payload,
              );
            } catch {
              // Best-effort tracing: never interfere with proxy traffic.
            }
          }
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
    try {
      const pkt = encodeDatagram(srcPort, dstIp, dstPort, payload);
      if (!dcSendSafe(this.channel, pkt)) return;

      // NetTracer's UDP pseudo-header currently only supports IPv4.
      // Skip tracing IPv6 datagrams until the format is extended.
      if (!dstIp.includes(":") && !(dstIp.startsWith("[") && dstIp.endsWith("]"))) {
        try {
          this.tracer?.recordUdpProxy("guest_to_remote", "webrtc", parseIpv4(dstIp), srcPort, dstPort, payload);
        } catch {
          // Best-effort tracing: never interfere with proxy traffic.
        }
      }
    } catch {
      // Drop invalid/oversized datagrams.
    }
  }
}
