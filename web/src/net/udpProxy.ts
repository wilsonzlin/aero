export type UdpProxyEvent = {
  srcIp: string;
  srcPort: number;
  dstPort: number;
  data: Uint8Array;
};

export type UdpProxyEventSink = (event: UdpProxyEvent) => void;

function encodeUdpProxyPacket(
  srcPort: number,
  dstIp: string,
  dstPort: number,
  payload: Uint8Array,
): Uint8Array {
  const ipParts = dstIp.split(".").map((s) => Number(s));
  if (ipParts.length !== 4 || ipParts.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) {
    throw new Error(`invalid IPv4 address: ${dstIp}`);
  }

  const out = new Uint8Array(2 + 4 + 2 + payload.length);
  const dv = new DataView(out.buffer);
  dv.setUint16(0, srcPort, false);
  out.set(ipParts as unknown as number[], 2);
  dv.setUint16(6, dstPort, false);
  out.set(payload, 8);
  return out;
}

/**
 * UDP proxy over WebSocket (fallback when WebRTC isn't available).
 *
 * Wire format:
 *   u16 src_port (guest port)
 *   u8[4] dst_ip
 *   u16 dst_port
 *   payload bytes...
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
      if (buf.length < 8) return;
      const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
      const srcPort = dv.getUint16(0, false);
      const srcIp = `${buf[2]}.${buf[3]}.${buf[4]}.${buf[5]}`;
      const dstPort = dv.getUint16(6, false);
      this.sink({ srcIp, srcPort, dstPort, data: buf.slice(8) });
    };
    this.ws = ws;
  }

  send(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    this.ws.send(encodeUdpProxyPacket(srcPort, dstIp, dstPort, payload));
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
      if (buf.length < 8) return;
      const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
      const srcPort = dv.getUint16(0, false);
      const srcIp = `${buf[2]}.${buf[3]}.${buf[4]}.${buf[5]}`;
      const dstPort = dv.getUint16(6, false);
      this.sink({ srcIp, srcPort, dstPort, data: buf.slice(8) });
    };
  }

  send(srcPort: number, dstIp: string, dstPort: number, payload: Uint8Array): void {
    if (this.channel.readyState !== "open") return;
    this.channel.send(encodeUdpProxyPacket(srcPort, dstIp, dstPort, payload));
  }
}

