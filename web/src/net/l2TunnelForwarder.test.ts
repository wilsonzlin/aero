import { describe, expect, it } from "vitest";

import { createIpcBuffer, openRingByKind } from "../ipc/ipc";
import { IO_IPC_NET_RX_QUEUE_KIND, IO_IPC_NET_TX_QUEUE_KIND } from "../runtime/shared_layout";
import { L2_TUNNEL_TYPE_FRAME, decodeL2Message, encodeL2Frame } from "../shared/l2TunnelProtocol";
import { L2_TUNNEL_SUBPROTOCOL, WebSocketL2TunnelClient } from "./l2Tunnel";
import { L2TunnelForwarder } from "./l2TunnelForwarder";

type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocket;

function microtask(): Promise<void> {
  return new Promise((resolve) => queueMicrotask(resolve));
}

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  static nextProtocol = L2_TUNNEL_SUBPROTOCOL;
  static last: FakeWebSocket | null = null;

  readonly url: string;
  readonly protocols?: string | string[];

  binaryType: BinaryType = "arraybuffer";
  bufferedAmount = 0;
  readyState = FakeWebSocket.CONNECTING;
  protocol = "";

  onopen: (() => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;

  readonly sent: Uint8Array[] = [];

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols = protocols;
    this.protocol = FakeWebSocket.nextProtocol;
    FakeWebSocket.last = this;
  }

  open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  send(data: string | ArrayBuffer | ArrayBufferView | Blob): void {
    if (typeof data === "string" || data instanceof Blob) throw new Error("unexpected ws send type");
    const view =
      data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    this.sent.push(view.slice());
  }

  close(code?: number, reason?: string): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({ code: code ?? 1000, reason: reason ?? "", wasClean: true } as CloseEvent);
  }

  emitMessage(payload: Uint8Array): void {
    const buf = payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength);
    this.onmessage?.({ data: buf } as MessageEvent);
  }
}

function resetFakeWebSocket(): void {
  FakeWebSocket.last = null;
}

function createNetRings(capTxBytes: number, capRxBytes: number): { netTx: ReturnType<typeof openRingByKind>; netRx: ReturnType<typeof openRingByKind> } {
  const { buffer } = createIpcBuffer([
    { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: capTxBytes },
    { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: capRxBytes },
  ]);
  return {
    netTx: openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND),
    netRx: openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND),
  };
}

describe("net/l2TunnelForwarder", () => {
  it("forwards guest->host frames from NET_TX to the tunnel", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;
    resetFakeWebSocket();
    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const { netTx, netRx } = createNetRings(256, 256);
    const forwarder = new L2TunnelForwarder(netTx, netRx);
    const tunnel = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", forwarder.sink, {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      forwarder.setTunnel(tunnel);
      forwarder.start();

      FakeWebSocket.last?.open();
      await microtask(); // allow open->flush scheduling

      const frame = Uint8Array.of(0, 1, 2, 3, 4, 5, 6, 7);
      expect(netTx.tryPush(frame)).toBe(true);

      forwarder.tick();
      await microtask(); // flush tunnel send queue

      const ws = FakeWebSocket.last;
      expect(ws).not.toBeNull();
      expect(ws!.sent.length).toBe(1);
      const msg = decodeL2Message(ws!.sent[0]!);
      expect(msg.type).toBe(L2_TUNNEL_TYPE_FRAME);
      expect(Array.from(msg.payload)).toEqual(Array.from(frame));
    } finally {
      forwarder.stop();
      if (original === undefined) delete (g as { WebSocket?: unknown }).WebSocket;
      else g.WebSocket = original;
    }
  });

  it("forwards host->guest FRAME messages from the tunnel to NET_RX", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;
    resetFakeWebSocket();
    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const { netTx, netRx } = createNetRings(256, 256);
    const forwarder = new L2TunnelForwarder(netTx, netRx);
    const tunnel = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", forwarder.sink, {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      forwarder.setTunnel(tunnel);
      forwarder.start();
      FakeWebSocket.last?.open();

      const frame = Uint8Array.of(9, 8, 7, 6, 5);
      FakeWebSocket.last?.emitMessage(encodeL2Frame(frame));

      forwarder.tick();

      const got = netRx.tryPop();
      expect(got && Array.from(got)).toEqual(Array.from(frame));
    } finally {
      forwarder.stop();
      if (original === undefined) delete (g as { WebSocket?: unknown }).WebSocket;
      else g.WebSocket = original;
    }
  });

  it("buffers inbound frames when NET_RX is full and enforces maxPendingRxBytes", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;
    resetFakeWebSocket();
    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const frameLen = 60;
    const { netTx, netRx } = createNetRings(256, 64);
    // Allow buffering exactly one frame (60B). The third injected frame should be dropped.
    const forwarder = new L2TunnelForwarder(netTx, netRx, { maxPendingRxBytes: frameLen });
    const tunnel = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", forwarder.sink, {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      forwarder.setTunnel(tunnel);
      forwarder.start();
      FakeWebSocket.last?.open();

      const f1 = new Uint8Array(frameLen).fill(1);
      const f2 = new Uint8Array(frameLen).fill(2);
      const f3 = new Uint8Array(frameLen).fill(3);

      FakeWebSocket.last?.emitMessage(encodeL2Frame(f1));
      FakeWebSocket.last?.emitMessage(encodeL2Frame(f2));
      FakeWebSocket.last?.emitMessage(encodeL2Frame(f3));

      // Only f1 fits in the ring; the rest must be pending/dropped.
      let got = netRx.tryPop();
      expect(got && Array.from(got)).toEqual(Array.from(f1));

      forwarder.tick(); // flush pending (should enqueue f2)
      got = netRx.tryPop();
      expect(got && Array.from(got)).toEqual(Array.from(f2));

      forwarder.tick(); // pending should be empty now (f3 dropped)
      expect(netRx.tryPop()).toBeNull();

      const stats = forwarder.stats();
      expect(stats.rxDroppedPendingOverflow).toBe(1);
      expect(stats.rxPendingBytes).toBe(0);
      expect(stats.rxPendingFrames).toBe(0);
    } finally {
      forwarder.stop();
      if (original === undefined) delete (g as { WebSocket?: unknown }).WebSocket;
      else g.WebSocket = original;
    }
  });
});
