import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram, encodeUdpRelayV2Datagram } from "../shared/udpRelayProtocol";
import { parseSignalMessageJSON } from "../shared/udpRelaySignaling";

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

function encodeDatagram(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): Uint8Array {
  if (dstIp.includes(":") || (dstIp.startsWith("[") && dstIp.endsWith("]"))) {
    const ip6 = parseIpv6(dstIp);
    return encodeUdpRelayV2Datagram({
      guestPort: srcPort,
      remoteIp: ip6,
      remotePort: dstPort,
      payload,
    });
  }
  return encodeUdpRelayV1Datagram({
    guestPort: srcPort,
    remoteIpv4: parseIpv4(dstIp),
    remotePort: dstPort,
    payload,
  });
}

/**
 * UDP proxy over WebSocket (fallback when WebRTC isn't available).
 *
 * Wire format is the same as the WebRTC UDP relay datagram framing. See:
 * - proxy/webrtc-udp-relay/PROTOCOL.md
 */
export class WebSocketUdpProxyClient {
  private ws: WebSocket | null = null;
  private connectPromise: Promise<void> | null = null;
  private authAccepted = false;

  constructor(
    private readonly proxyBaseUrl: string,
    private readonly sink: UdpProxyEventSink,
    private readonly authToken?: string,
  ) {}

  connect(): Promise<void> {
    if (this.ws && this.ws.readyState === WebSocket.OPEN && (!this.authToken || this.authAccepted)) {
      return Promise.resolve();
    }
    if (this.connectPromise) return this.connectPromise;

    this.connectPromise = (this.authToken
      ? this.connectWithAuth({ includeQueryAuth: false, sendAuthMessage: true }).catch(() =>
          // Query-string auth is a compatibility fallback for environments where
          // a first-message auth handshake is not supported.
          this.connectWithAuth({ includeQueryAuth: true, sendAuthMessage: false }),
        )
      : this.connectWithAuth({ includeQueryAuth: false, sendAuthMessage: false }))
      .finally(() => {
        this.connectPromise = null;
      });

    return this.connectPromise;
  }

  private connectWithAuth(opts: { includeQueryAuth: boolean; sendAuthMessage: boolean }): Promise<void> {
    this.close();
    this.authAccepted = false;

    const url = new URL(this.proxyBaseUrl);
    if (url.protocol === "http:") url.protocol = "ws:";
    else if (url.protocol === "https:") url.protocol = "wss:";
    url.pathname = `${url.pathname.replace(/\/$/, "")}/udp`;
    if (this.authToken && opts.includeQueryAuth) {
      // Forward/compat: support both jwt token and api_key query param names.
      url.searchParams.set("token", this.authToken);
      url.searchParams.set("apiKey", this.authToken);
    }

    const ws = new WebSocket(url.toString());
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    return new Promise((resolve, reject) => {
      let settled = false;
      let graceTimer: ReturnType<typeof setTimeout> | null = null;

      const settle = (err?: unknown) => {
        if (settled) return;
        settled = true;
        if (graceTimer) clearTimeout(graceTimer);
        graceTimer = null;
        if (err) {
          reject(err);
        } else {
          resolve();
        }
      };

      ws.onopen = () => {
        if (!this.authToken) {
          this.authAccepted = true;
          settle();
          return;
        }

        if (opts.sendAuthMessage) {
          try {
            ws.send(JSON.stringify({ type: "auth", token: this.authToken, apiKey: this.authToken }));
          } catch {
            // Ignore; we'll fail if the socket closes.
          }
        }

        // Some relay builds do not send an explicit auth acknowledgement for the
        // UDP WebSocket. Give the connection a small grace period to fail fast
        // on invalid credentials, then treat it as connected.
        graceTimer = setTimeout(() => {
          if (ws.readyState === WebSocket.OPEN) {
            this.authAccepted = true;
            settle();
          }
        }, 100);
      };

      ws.onerror = () => {
        if (!this.authAccepted) settle(new Error("udp websocket error"));
      };

      ws.onclose = (evt) => {
        if (!this.authAccepted) {
          settle(new Error(`udp websocket closed (${evt.code}): ${evt.reason}`));
        }
      };

      ws.onmessage = (evt) => {
        if (evt.data instanceof ArrayBuffer) {
          if (!this.authAccepted) {
            this.authAccepted = true;
            settle();
          }

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
          return;
        }

        if (typeof evt.data === "string") {
          // Control plane messages (auth / errors) are text frames.
          try {
            const msg = parseSignalMessageJSON(evt.data);
            if (msg.type === "error") {
              settle(new Error(`udp websocket error (${msg.code}): ${msg.message}`));
            } else if (msg.type === "auth") {
              this.authAccepted = true;
              settle();
            }
          } catch {
            // Ignore unknown text messages.
          }
        }
      };
    });
  }

  send(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    try {
      this.ws.send(encodeDatagram(srcPort, dstIp, dstPort, payload));
    } catch {
      // Drop invalid/oversized datagrams.
    }
  }

  close(): void {
    this.ws?.close();
    this.ws = null;
    this.authAccepted = false;
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
