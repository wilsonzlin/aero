import { describe, expect, it } from "vitest";

import { createIpcBuffer, openRingByKind } from "../ipc/ipc";
import { decodeEvent, encodeEvent } from "../ipc/protocol";
import { IO_IPC_NET_RX_QUEUE_KIND, IO_IPC_NET_TX_QUEUE_KIND } from "../runtime/shared_layout";
import { L2_TUNNEL_TYPE_FRAME, decodeL2Message, encodeL2Frame } from "../shared/l2TunnelProtocol";
import { L2_TUNNEL_SUBPROTOCOL, WebSocketL2TunnelClient, type L2TunnelClient } from "./l2Tunnel";
import {
  L2TunnelForwarder,
  computeL2TunnelForwarderDropDeltas,
  formatL2TunnelForwarderLog,
  type L2TunnelForwarderStats,
} from "./l2TunnelForwarder";

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

function createNetRings(
  capTxBytes: number,
  capRxBytes: number,
): { netTx: ReturnType<typeof openRingByKind>; netRx: ReturnType<typeof openRingByKind> } {
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

  it("invokes onFrame hook for guest TX/RX frames", () => {
    const { netTx, netRx } = createNetRings(256, 256);
    const observed: { dir: string; payload: number[] }[] = [];
    const forwarder = new L2TunnelForwarder(netTx, netRx);
    forwarder.setOnFrame((ev) => observed.push({ dir: ev.direction, payload: Array.from(ev.frame) }));

    const tunnel: L2TunnelClient = {
      connect: () => {},
      close: () => {},
      sendFrame: () => true,
    };

    forwarder.setTunnel(tunnel);
    forwarder.start();

    const tx = Uint8Array.of(1, 2, 3);
    expect(netTx.tryPush(tx)).toBe(true);
    forwarder.tick();

    const rx = Uint8Array.of(4, 5);
    forwarder.sink({ type: "frame", frame: rx });

    expect(observed).toEqual([
      { dir: "guest_tx", payload: [1, 2, 3] },
      { dir: "guest_rx", payload: [4, 5] },
    ]);
    forwarder.stop();
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
      // Ring was full at least once while buffering.
      expect(stats.rxRingFull).toBeGreaterThan(0);
    } finally {
      forwarder.stop();
      if (original === undefined) delete (g as { WebSocket?: unknown }).WebSocket;
      else g.WebSocket = original;
    }
  });

  it("drops inbound frames when NET_RX is full and maxPendingRxBytes=0", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;
    resetFakeWebSocket();
    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const frameLen = 60;
    const { netTx, netRx } = createNetRings(256, 64);
    const forwarder = new L2TunnelForwarder(netTx, netRx, { maxPendingRxBytes: 0 });
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

      FakeWebSocket.last?.emitMessage(encodeL2Frame(f1));
      FakeWebSocket.last?.emitMessage(encodeL2Frame(f2));

      // f1 fits; f2 should be dropped (no pending).
      const got = netRx.tryPop();
      expect(got && Array.from(got)).toEqual(Array.from(f1));
      expect(netRx.tryPop()).toBeNull();

      const stats = forwarder.stats();
      expect(stats.rxDroppedNetRxFull).toBe(1);
      expect(stats.rxPendingFrames).toBe(0);
      expect(stats.rxPendingBytes).toBe(0);
    } finally {
      forwarder.stop();
      if (original === undefined) delete (g as { WebSocket?: unknown }).WebSocket;
      else g.WebSocket = original;
    }
  });

  it("counts tunnel send backpressure and stops draining NET_TX", () => {
    const { netTx, netRx } = createNetRings(256, 256);
    const forwarder = new L2TunnelForwarder(netTx, netRx);

    let calls = 0;
    const sent: number[] = [];
    const tunnel: L2TunnelClient = {
      connect() {},
      sendFrame(frame: Uint8Array): boolean {
        calls += 1;
        if (calls === 2) return false;
        sent.push(frame[0] ?? 0);
        return true;
      },
      close() {},
    };

    forwarder.setTunnel(tunnel);
    forwarder.start();

    expect(netTx.tryPush(Uint8Array.of(1))).toBe(true);
    expect(netTx.tryPush(Uint8Array.of(2))).toBe(true);
    expect(netTx.tryPush(Uint8Array.of(3))).toBe(true);

    forwarder.tick();

    expect(sent).toEqual([1]);
    // Third frame should remain in NET_TX since we stop draining after observing backpressure.
    expect(Array.from(netTx.tryPop() ?? [])).toEqual([3]);
    expect(netTx.tryPop()).toBeNull();

    const stats = forwarder.stats();
    expect(stats.txFrames).toBe(1);
    expect(stats.txBytes).toBe(1);
    expect(stats.txDroppedTunnelBackpressure).toBe(1);
  });

  it("formats an io.worker-friendly log string and roundtrips through encode/decode", () => {
    const stats: L2TunnelForwarderStats = {
      running: true,
      txFrames: 10,
      txBytes: 100,
      txRingEmpty: 0,
      txDroppedNoTunnel: 0,
      txDroppedSendError: 5,
      txDroppedTunnelBackpressure: 5,
      rxFrames: 20,
      rxBytes: 200,
      rxRingFull: 1,
      rxPendingFrames: 6,
      rxPendingBytes: 600,
      rxDroppedNetRxFull: 3,
      rxDroppedPendingOverflow: 4,
      rxDroppedRingTooSmall: 0,
      rxDroppedWhileStopped: 0,
    };

    const drops = computeL2TunnelForwarderDropDeltas(null, stats);
    const msg = formatL2TunnelForwarderLog({ connection: "open", stats, dropsSinceLast: drops });
    expect(msg).toContain("l2: open");
    expect(msg).toContain("tx=10f/100B");
    expect(msg).toContain("rx=20f/200B");
    expect(msg).toContain("drop+{");

    const encoded = encodeEvent({ kind: "log", level: "info", message: msg });
    const decoded = decodeEvent(encoded);
    expect(decoded.kind).toBe("log");
    expect(decoded).toMatchObject({ level: "info", message: msg });
  });
});
