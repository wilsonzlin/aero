import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram } from "../shared/udpRelayProtocol";

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

/**
 * UDP proxy over WebSocket (fallback when WebRTC isn't available).
 *
 * Wire format is the same as the WebRTC UDP relay datagram framing. See:
 * - proxy/webrtc-udp-relay/PROTOCOL.md
 */
export class WebSocketUdpProxyClient {
  private ws: WebSocket | null = null;

  constructor(
    private readonly proxyBaseUrl: string,
    private readonly sink: UdpProxyEventSink,
  ) {}

  connect(): void {
    if (this.ws) return;

    const url = new URL(this.proxyBaseUrl);
    url.pathname = `${url.pathname.replace(/\/$/, "")}/udp`;

    const ws = new WebSocket(url.toString());
    ws.binaryType = "arraybuffer";
    ws.onmessage = (evt) => {
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
    this.ws = ws;
  }

  send(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    try {
      this.ws.send(
        encodeUdpRelayV1Datagram({
          guestPort: srcPort,
          remoteIpv4: parseIpv4(dstIp),
          remotePort: dstPort,
          payload,
        }),
      );
    } catch {
      // Drop invalid/oversized datagrams.
    }
  }

  close(): void {
    this.ws?.close();
    this.ws = null;
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
      this.channel.send(
        encodeUdpRelayV1Datagram({
          guestPort: srcPort,
          remoteIpv4: parseIpv4(dstIp),
          remotePort: dstPort,
          payload,
        }),
      );
    } catch {
      // Drop invalid/oversized datagrams.
    }
  }
}
