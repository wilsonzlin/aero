import { afterEach, describe, expect, it, vi } from "vitest";

import {
  L2_TUNNEL_DATA_CHANNEL_LABEL,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX,
  L2_TUNNEL_VERSION,
  decodeL2Message,
  encodeError,
  encodeL2Frame,
  encodePing,
  encodeStructuredErrorPayload,
} from "../shared/l2TunnelProtocol.ts";
import { L2_TUNNEL_SUBPROTOCOL, WebRtcL2TunnelClient, WebSocketL2TunnelClient, type L2TunnelEvent } from "./l2Tunnel.ts";

type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocket;

function microtask(): Promise<void> {
  return new Promise((resolve) => queueMicrotask(resolve));
}

const FAKE_NOW_MS = 1_000_000;

function useDeterministicFakeTimers(): void {
  vi.useFakeTimers();
  // Under fake timers, `Date.now()` defaults to 0 which can interact badly with
  // throttled error emission (e.g. `errorIntervalMs`). Set an explicit fake
  // wall-clock so first errors are not accidentally suppressed.
  vi.setSystemTime(FAKE_NOW_MS);
}

class FakeRtcDataChannel {
  label = L2_TUNNEL_DATA_CHANNEL_LABEL;
  ordered = true;
  maxRetransmits: number | null = null;
  maxPacketLifeTime: number | null = null;
  binaryType: BinaryType = "arraybuffer";
  bufferedAmount = 0;
  bufferedAmountLowThreshold = 0;
  readyState: RTCDataChannelState = "open";

  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onclose: ((ev: Event) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onbufferedamountlow: ((ev: Event) => void) | null = null;

  readonly sent: Uint8Array[] = [];

  send(data: string | Blob | ArrayBuffer | ArrayBufferView): void {
    if (typeof data === "string") throw new Error("unexpected string send");
    if (data instanceof Blob) throw new Error("unexpected Blob send");
    const view = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    // Copy to avoid aliasing mutations.
    this.sent.push(view.slice());
  }

  close(): void {
    this.readyState = "closed";
    this.onclose?.(new Event("close"));
  }

  emitMessage(payload: Uint8Array): void {
    const buf = payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength);
    this.onmessage?.({ data: buf } as MessageEvent);
  }
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
  lastClose?: { code?: number; reason?: string };

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
    this.lastClose = { code, reason };
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({ code: code ?? 1000, reason: reason ?? "", wasClean: true } as CloseEvent);
  }
}

function resetFakeWebSocket(): void {
  FakeWebSocket.last = null;
}

describe("net/l2Tunnel", () => {
  // Safety net: a failing test that forgets to restore fake timers can poison
  // the rest of the suite (e.g. tests that await real `setTimeout`).
  afterEach(() => {
    vi.useRealTimers();
  });

  it("rejects unreliable RTCDataChannels", () => {
    const channel = new FakeRtcDataChannel();
    channel.maxRetransmits = 0;
    expect(() => new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, () => {})).toThrow(/maxRetransmits/);
  });

  it("rejects partially reliable RTCDataChannels", () => {
    const channel = new FakeRtcDataChannel();
    channel.maxRetransmits = 0;
    expect(() => new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, () => {})).toThrow(/maxRetransmits/);
  });

  it("rejects partially reliable RTCDataChannels (maxPacketLifeTime)", () => {
    const channel = new FakeRtcDataChannel();
    channel.maxPacketLifeTime = 0;
    expect(() => new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, () => {})).toThrow(/maxPacketLifeTime/);
  });
  it("forwards FRAME messages and responds to PING", async () => {
    const channel = new FakeRtcDataChannel();
    const events: L2TunnelEvent[] = [];

    const client = new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, (ev) => events.push(ev), {
      // Keepalive timers would keep the vitest process alive; we close at the end
      // of the test, but set a large interval to avoid flakiness if close breaks.
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      await microtask(); // allow constructor-scheduled `open`
      expect(events[0]?.type).toBe("open");

      const frame = Uint8Array.of(1, 2, 3, 4);
      client.sendFrame(frame);
      await microtask(); // flush outbound queue

      expect(channel.sent.length).toBe(1);
      const out = decodeL2Message(channel.sent[0]!);
      expect(out.type).toBe(L2_TUNNEL_TYPE_FRAME);
      expect(Array.from(out.payload)).toEqual(Array.from(frame));

      const inboundFrame = Uint8Array.of(9, 8, 7);
      channel.emitMessage(encodeL2Frame(inboundFrame));
      const frameEvent = events.find((e) => (e as { type?: string }).type === "frame") as { frame?: Uint8Array } | undefined;
      expect(frameEvent?.frame && Array.from(frameEvent.frame)).toEqual(Array.from(inboundFrame));

      const pingPayload = new Uint8Array(4);
      new DataView(pingPayload.buffer).setUint32(0, 123, false);
      channel.emitMessage(encodePing(pingPayload));
      await microtask(); // flush pong response

      expect(channel.sent.length).toBe(2);
      const pong = decodeL2Message(channel.sent[1]!);
      expect(pong.type).toBe(L2_TUNNEL_TYPE_PONG);
      expect(Array.from(pong.payload)).toEqual(Array.from(pingPayload));
    } finally {
      client.close();
    }
  });

  it("decodes structured ERROR payloads", async () => {
    const channel = new FakeRtcDataChannel();
    const events: L2TunnelEvent[] = [];

    const client = new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      await microtask();

      const msg = "blocked by policy";
      const payload = encodeStructuredErrorPayload(1234, msg, 256);
      const wire = encodeError(payload);

      channel.emitMessage(wire);

      const errEv = events.find((e) => (e as { type?: string }).type === "error") as { error?: unknown } | undefined;
      expect(errEv?.error).toBeInstanceOf(Error);
      const message = (errEv?.error as Error | undefined)?.message ?? "";
      expect(message).toContain("1234");
      // Peer-provided error messages are untrusted; do not reflect them in UI-visible errors.
      expect(message).not.toContain(msg);
    } finally {
      client.close();
    }
  });

  it("WebSocket client appends /l2 and requires aero-l2-tunnel-v1 subprotocol", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    // Ensure a deterministic negotiated protocol for this test.
    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();

    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("https://gateway.example.com/base", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/base/l2");
      expect(FakeWebSocket.last?.protocols).toBe(L2_TUNNEL_SUBPROTOCOL);

      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client accepts same-origin /path base URLs when location.href is available", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWs = g.WebSocket;
    const originalLocation = (g as { location?: unknown }).location;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;
    (g as { location?: unknown }).location = { href: "https://gateway.example.com/app/index.html" };

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("/base", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/base/l2");
      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (originalWs === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = originalWs;
      }
      if (originalLocation === undefined) {
        delete (g as { location?: unknown }).location;
      } else {
        (g as { location?: unknown }).location = originalLocation;
      }
    }
  });

  it("disables keepalive when keepaliveMinMs=keepaliveMaxMs=0", async () => {
    useDeterministicFakeTimers();
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 0,
      keepaliveMaxMs: 0,
    });

    try {
      client.connect();
      FakeWebSocket.last?.open();
      await microtask();
      expect(events[0]?.type).toBe("open");

      // Advance fake time; with keepalive disabled we should not emit any PINGs.
      await vi.advanceTimersByTimeAsync(50);
      await microtask();
      expect(FakeWebSocket.last?.sent.length).toBe(0);
    } finally {
      client.close();
      vi.useRealTimers();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("honors maxControlSize for PING/PONG payloads", async () => {
    const channel = new FakeRtcDataChannel();
    const events: L2TunnelEvent[] = [];

    const client = new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      maxControlSize: 1024,
    });

    try {
      await microtask();
      expect(events[0]?.type).toBe("open");

      const bigPayload = new Uint8Array(512);
      bigPayload.fill(0xaa);
      channel.emitMessage(encodePing(bigPayload, { maxPayload: 1024 }));
      await microtask();

      expect(channel.sent.length).toBe(1);
      const pong = decodeL2Message(channel.sent[0]!, { maxControlPayload: 1024 });
      expect(pong.type).toBe(L2_TUNNEL_TYPE_PONG);
      expect(pong.payload.length).toBe(bigPayload.length);
      expect(Buffer.from(pong.payload)).toEqual(Buffer.from(bigPayload));
    } finally {
      client.close();
    }
  });

  it("sends keepalive PINGs with empty payload when maxControlSize < 4", async () => {
    useDeterministicFakeTimers();
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 10,
      keepaliveMaxMs: 10,
      maxControlSize: 0,
    });

    try {
      // Keepalive delay is randomized within [min,max]; we set them equal for determinism.
      // `sendPing()` enqueues, then flushes via `queueMicrotask`, so await a microtask after
      // advancing fake time.
      client.connect();
      FakeWebSocket.last?.open();
      await microtask();
      expect(events[0]?.type).toBe("open");

      await vi.advanceTimersByTimeAsync(10);
      await microtask();

      expect(FakeWebSocket.last?.sent.length).toBe(1);
      const ping = decodeL2Message(FakeWebSocket.last!.sent[0]!, { maxControlPayload: 0 });
      expect(ping.type).toBe(L2_TUNNEL_TYPE_PING);
      expect(ping.payload.length).toBe(0);
    } finally {
      client.close();
      vi.useRealTimers();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("closes the tunnel when keepalive pings go unanswered", async () => {
    useDeterministicFakeTimers();
    // `BaseL2TunnelClient` uses `performance.now()` for idle/RTT timing when available. Ensure
    // the clock source advances with fake timers even in environments where `performance.now`
    // is not patched by the timer shim.
    const perfNowSpy =
      typeof performance !== "undefined" ? vi.spyOn(performance, "now").mockImplementation(() => Date.now()) : null;
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 10,
      keepaliveMaxMs: 10,
    });

    try {
      client.connect();
      FakeWebSocket.last?.open();
      await microtask();
      expect(events[0]?.type).toBe("open");

      // The client closes when it has been idle for `keepaliveMaxMs * 2` without receiving
      // any inbound traffic. With keepaliveMinMs=keepaliveMaxMs=10, the timeout is 20ms,
      // checked on each ping tick: it sends PINGs at 10ms and 20ms, then at 30ms it becomes
      // > 20ms and closes (without sending a third PING).
      await vi.advanceTimersByTimeAsync(10);
      await microtask();
      expect(FakeWebSocket.last?.sent.length).toBe(1);

      await vi.advanceTimersByTimeAsync(10);
      await microtask();
      expect(FakeWebSocket.last?.sent.length).toBe(2);

      await vi.advanceTimersByTimeAsync(10);
      await microtask();
      expect(FakeWebSocket.last?.sent.length).toBe(2);

      expect(events.some((ev) => ev.type === "close")).toBe(true);
      const errEv = events.find((ev) => ev.type === "error") as { error?: unknown } | undefined;
      expect(errEv?.error).toBeInstanceOf(Error);
      expect((errEv?.error as Error | undefined)?.message).toContain("keepalive timeout");
    } finally {
      client.close();
      perfNowSpy?.mockRestore();
      vi.useRealTimers();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client sends token via query string by default", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;
    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      token: "sekrit",
    });

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/l2?token=sekrit");
      expect(FakeWebSocket.last?.protocols).toBe(L2_TUNNEL_SUBPROTOCOL);

      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client drops apiKey and overwrites token in query string auth mode", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient(
      "wss://gateway.example.com/l2?token=old&apiKey=old&foo=bar",
      (ev) => events.push(ev),
      {
        keepaliveMinMs: 60_000,
        keepaliveMaxMs: 60_000,
        token: "sekrit",
      },
    );

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/l2?token=sekrit&foo=bar");
      expect(FakeWebSocket.last?.protocols).toBe(L2_TUNNEL_SUBPROTOCOL);

      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client can send token via Sec-WebSocket-Protocol", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      token: "sekrit",
      tokenTransport: "subprotocol",
    });

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/l2");
      expect(FakeWebSocket.last?.protocols).toEqual([
        L2_TUNNEL_SUBPROTOCOL,
        `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}sekrit`,
      ]);

      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client drops ?token= when using tokenTransport=subprotocol", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient(
      "wss://gateway.example.com/l2?token=old&apiKey=old&foo=bar",
      (ev) => events.push(ev),
      {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      token: "sekrit",
      tokenTransport: "subprotocol",
      },
    );

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/l2?foo=bar");
      expect(FakeWebSocket.last?.protocols).toEqual([
        L2_TUNNEL_SUBPROTOCOL,
        `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}sekrit`,
      ]);

      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client can send token via both query params and Sec-WebSocket-Protocol", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      token: "sekrit",
      tokenTransport: "both",
    });

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/l2?token=sekrit");
      expect(FakeWebSocket.last?.protocols).toEqual([
        L2_TUNNEL_SUBPROTOCOL,
        `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}sekrit`,
      ]);

      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client drops apiKey and overwrites token when tokenTransport=both", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient(
      "wss://gateway.example.com/l2?token=old&apiKey=old&foo=bar",
      (ev) => events.push(ev),
      {
        keepaliveMinMs: 60_000,
        keepaliveMaxMs: 60_000,
        token: "sekrit",
        tokenTransport: "both",
      },
    );

    try {
      client.connect();
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/l2?token=sekrit&foo=bar");
      expect(FakeWebSocket.last?.protocols).toEqual([
        L2_TUNNEL_SUBPROTOCOL,
        `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}sekrit`,
      ]);

      FakeWebSocket.last?.open();
      expect(events[0]?.type).toBe("open");
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("WebSocket client rejects negotiated token subprotocol (must negotiate aero-l2-tunnel-v1)", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}sekrit`;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      token: "sekrit",
      tokenTransport: "subprotocol",
    });

    try {
      client.connect();
      FakeWebSocket.last?.open();

      const errEv = events.find((e) => (e as { type?: string }).type === "error") as { error?: unknown } | undefined;
      expect(errEv?.error).toBeInstanceOf(Error);

      const closeEv = events.find((e) => (e as { type?: string }).type === "close") as { code?: number } | undefined;
      expect(closeEv?.code).toBe(1002);
    } finally {
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("rejects tokens that cannot be represented in Sec-WebSocket-Protocol", async () => {
    expect(
      () =>
        new WebSocketL2TunnelClient("wss://gateway.example.com/l2", () => {}, {
          token: "not a token",
          tokenTransport: "subprotocol",
        }),
    ).toThrow(/Sec-WebSocket-Protocol/);
  });

  it("rejects invalid tokenTransport values at runtime", () => {
    expect(
      () =>
        new WebSocketL2TunnelClient("wss://gateway.example.com/l2", () => {}, {
          token: "sekrit",
          tokenTransport: "nope" as unknown as "query",
        }),
    ).toThrow(/tokenTransport/);
  });

  it("WebSocket client closes when subprotocol is not negotiated", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = "";
    resetFakeWebSocket();

    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      client.connect();
      FakeWebSocket.last?.open();

      const errEv = events.find((e) => (e as { type?: string }).type === "error") as { error?: unknown } | undefined;
      expect(errEv?.error).toBeInstanceOf(Error);

      const closeEv = events.find((e) => (e as { type?: string }).type === "close") as { code?: number } | undefined;
      expect(closeEv?.code).toBe(1002);
    } finally {
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("queues outbound frames while bufferedAmount exceeds maxBufferedAmount", async () => {
    useDeterministicFakeTimers();
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();

    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      maxBufferedAmount: 0,
    });

    try {
      client.connect();
      FakeWebSocket.last!.bufferedAmount = 1;
      FakeWebSocket.last!.open();

      client.sendFrame(Uint8Array.of(1, 2, 3));
      await microtask();

      expect(FakeWebSocket.last!.sent.length).toBe(0);

      FakeWebSocket.last!.bufferedAmount = 0;
      // Drain retry is scheduled via `setTimeout` in the tunnel client; advance fake timers
      // past the retry delay and then flush the queued microtask that performs the send.
      await vi.advanceTimersByTimeAsync(20);
      await microtask();

      // Frame should flush once the socket drains.
      expect(FakeWebSocket.last!.sent.length).toBe(1);
    } finally {
      client.close();
      vi.useRealTimers();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("sendFrame() returns false when the WebSocket client is not connected", () => {
    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      // No need to connect; keepalive timers are never armed.
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    expect(client.sendFrame(Uint8Array.of(1, 2, 3))).toBe(false);
    const errEv = events.find((e) => (e as { type?: string }).type === "error") as { error?: unknown } | undefined;
    expect(errEv?.error).toBeInstanceOf(Error);
  });

  it("sendFrame() returns false when the outbound queue overflows", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const original = g.WebSocket;

    FakeWebSocket.nextProtocol = L2_TUNNEL_SUBPROTOCOL;
    resetFakeWebSocket();
    g.WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const client = new WebSocketL2TunnelClient("wss://gateway.example.com/l2", (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
      maxQueuedBytes: 0,
    });

    try {
      client.connect();
      // Do not open the socket; we only need the client to have a transport so
      // `canEnqueue()` passes and queue overflow logic runs.
      expect(client.sendFrame(Uint8Array.of(1))).toBe(false);
      const errEv = events.find((e) => (e as { type?: string }).type === "error") as { error?: unknown } | undefined;
      expect(errEv?.error).toBeInstanceOf(Error);
    } finally {
      client.close();
      if (original === undefined) {
        delete (g as { WebSocket?: unknown }).WebSocket;
      } else {
        g.WebSocket = original;
      }
    }
  });

  it("sendFrame() returns false after closing the WebRTC client", async () => {
    const channel = new FakeRtcDataChannel();
    const events: L2TunnelEvent[] = [];

    const client = new WebRtcL2TunnelClient(channel as unknown as RTCDataChannel, (ev) => events.push(ev), {
      keepaliveMinMs: 60_000,
      keepaliveMaxMs: 60_000,
    });

    try {
      await microtask();
      expect(events[0]?.type).toBe("open");
      expect(client.sendFrame(Uint8Array.of(1))).toBe(true);
      client.close();
      expect(client.sendFrame(Uint8Array.of(2))).toBe(false);
    } finally {
      client.close();
    }
  });
});
