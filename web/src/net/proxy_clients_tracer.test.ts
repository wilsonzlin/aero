import { describe, expect, it } from "vitest";

import { encodeUdpRelayV1Datagram } from "../shared/udpRelayProtocol";
import { NetTracer } from "./net_tracer";
import { ascii, parsePcapng, readU16LE } from "./net_tracer_test_helpers";
import { WebSocketTcpProxyClient } from "./tcpProxy";
import { WebRtcUdpProxyClient, WebSocketUdpProxyClient } from "./udpProxy";

function toArrayBuffer(view: Uint8Array): ArrayBuffer {
  // `Uint8Array.buffer` is `ArrayBufferLike` (can be `SharedArrayBuffer`), but the WebSocket
  // codepaths exercised by these tests expect a plain `ArrayBuffer`. Copy to guarantee an
  // `ArrayBuffer` return type even if the source view is backed by shared memory.
  const out = new Uint8Array(view.byteLength);
  out.set(view);
  return out.buffer;
}

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  static last: FakeWebSocket | null = null;

  readonly url: string;
  readyState = FakeWebSocket.CONNECTING;
  binaryType: BinaryType = "blob";

  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;

  sent: unknown[] = [];

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.last = this;
  }

  send(data: unknown): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({} as CloseEvent);
  }

  triggerOpen(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.({} as Event);
  }

  triggerMessage(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

class FakeDataChannel {
  binaryType: BinaryType = "blob";
  readyState: RTCDataChannelState = "open";
  onmessage: ((ev: MessageEvent) => void) | null = null;
  sent: unknown[] = [];

  send(data: unknown): void {
    this.sent.push(data);
  }

  triggerMessage(data: ArrayBuffer): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

describe("NetTracer integration (proxy clients)", () => {
  it("records WebSocketTcpProxyClient send/receive as ATCP pseudo packets", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const tracer = new NetTracer({ captureTcpProxy: true });
      tracer.enable();

      const client = new WebSocketTcpProxyClient("ws://gateway.example.com", () => {}, { tracer });
      client.connect(7, "127.0.0.1", 80);

      expect(FakeWebSocket.last).not.toBeNull();
      const ws = FakeWebSocket.last!;
      ws.triggerOpen();

      client.send(7, Uint8Array.of(1, 2, 3));
      ws.triggerMessage(Uint8Array.of(4, 5).buffer);

      const { epbs } = parsePcapng(tracer.exportPcapng());
      const atcp = epbs.filter((epb) => ascii(epb.packetData.slice(0, 4)) === "ATCP").map((epb) => epb.packetData);
      expect(atcp.length).toBeGreaterThanOrEqual(2);

      // Direction is encoded both in the pseudo-header and EPB flags; we only
      // validate the pseudo-header here.
      expect(atcp.some((p) => p[4] === 0)).toBe(true); // guest_to_remote
      expect(atcp.some((p) => p[4] === 1)).toBe(true); // remote_to_guest
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("records WebSocketUdpProxyClient IPv4 datagrams as AUDP pseudo packets (transport=proxy)", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const tracer = new NetTracer({ captureUdpProxy: true });
      tracer.enable();

      const client = new WebSocketUdpProxyClient("ws://gateway.example.com", () => {}, { tracer });
      const connectPromise = client.connect();

      expect(FakeWebSocket.last).not.toBeNull();
      const ws = FakeWebSocket.last!;
      ws.triggerOpen();
      ws.triggerMessage(JSON.stringify({ type: "ready" }));
      await connectPromise;

      client.send(1234, "203.0.113.9", 53, Uint8Array.of(9, 9, 9));

      const inbound = encodeUdpRelayV1Datagram({
        guestPort: 1234,
        remoteIpv4: [203, 0, 113, 9],
        remotePort: 53,
        payload: Uint8Array.of(1, 2, 3),
      });
      ws.triggerMessage(toArrayBuffer(inbound));

      const { epbs } = parsePcapng(tracer.exportPcapng());
      const audp = epbs.filter((epb) => ascii(epb.packetData.slice(0, 4)) === "AUDP").map((epb) => epb.packetData);
      expect(audp.length).toBeGreaterThanOrEqual(2);

      const outbound = audp.find((p) => p[4] === 0);
      const inboundPkt = audp.find((p) => p[4] === 1);
      expect(outbound).toBeTruthy();
      expect(inboundPkt).toBeTruthy();

      // transport byte: 0=webrtc, 1=proxy
      expect(outbound![5]).toBe(1);
      expect(inboundPkt![5]).toBe(1);

      // sanity-check ports are little-endian in the pseudo-header.
      expect(readU16LE(outbound!, 12)).toBe(1234);
      expect(readU16LE(outbound!, 14)).toBe(53);
      expect(readU16LE(inboundPkt!, 12)).toBe(53);
      expect(readU16LE(inboundPkt!, 14)).toBe(1234);
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("records WebRtcUdpProxyClient IPv4 datagrams as AUDP pseudo packets (transport=webrtc)", () => {
    const tracer = new NetTracer({ captureUdpProxy: true });
    tracer.enable();

    const dc = new FakeDataChannel();
    const udp = new WebRtcUdpProxyClient(dc as unknown as RTCDataChannel, () => {}, { tracer });

    udp.send(5000, "192.0.2.1", 6000, Uint8Array.of(7, 7, 7));

    const inbound = encodeUdpRelayV1Datagram({
      guestPort: 5000,
      remoteIpv4: [192, 0, 2, 1],
      remotePort: 6000,
      payload: Uint8Array.of(1, 2),
    });
    dc.triggerMessage(toArrayBuffer(inbound));

    const { epbs } = parsePcapng(tracer.exportPcapng());
    const audp = epbs.filter((epb) => ascii(epb.packetData.slice(0, 4)) === "AUDP").map((epb) => epb.packetData);
    expect(audp.length).toBeGreaterThanOrEqual(2);

    expect(audp.some((p) => p[4] === 0 && p[5] === 0)).toBe(true); // outbound, webrtc
    expect(audp.some((p) => p[4] === 1 && p[5] === 0)).toBe(true); // inbound, webrtc
  });
});
