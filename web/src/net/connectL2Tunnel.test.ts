import { describe, expect, it, vi } from "vitest";

import { L2_TUNNEL_SUBPROTOCOL, type L2TunnelEvent } from "./l2Tunnel";
import { connectL2Tunnel } from "./connectL2Tunnel";

type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocket;

class FakeRtcDataChannel {
  label = "";
  readyState: RTCDataChannelState = "open";
  ordered = true;
  maxRetransmits: number | null = null;
  maxPacketLifeTime: number | null = null;

  binaryType: BinaryType = "arraybuffer";
  bufferedAmount = 0;
  bufferedAmountLowThreshold = 0;

  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: Event) => void) | null = null;
  onbufferedamountlow: ((ev: Event) => void) | null = null;

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  send(_data: string | ArrayBuffer | ArrayBufferView | Blob): void {
    // Intentionally empty for tests.
  }

  close(): void {
    this.readyState = "closed";
    this.onclose?.(new Event("close"));
  }
}

class FakePeerConnection {
  static last: FakePeerConnection | null = null;

  iceGatheringState: RTCIceGatheringState = "complete";
  localDescription: RTCSessionDescriptionInit | null = null;

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  constructor(_config: RTCConfiguration) {
    FakePeerConnection.last = this;
  }

  createDataChannel(label: string, init?: RTCDataChannelInit): RTCDataChannel {
    const dc = new FakeRtcDataChannel();
    dc.label = label;
    dc.ordered = init?.ordered ?? true;
    dc.maxRetransmits = init?.maxRetransmits ?? null;
    dc.maxPacketLifeTime = init?.maxPacketLifeTime ?? null;
    return dc as unknown as RTCDataChannel;
  }

  async createOffer(): Promise<RTCSessionDescriptionInit> {
    return { type: "offer", sdp: "fake-offer" };
  }

  async setLocalDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.localDescription = desc;
  }

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  async setRemoteDescription(_desc: RTCSessionDescriptionInit): Promise<void> {}

  close(): void {}
}

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  static last: FakeWebSocket | null = null;
  static instances: FakeWebSocket[] = [];

  readonly url: string;
  readonly protocols?: string | string[];

  binaryType: BinaryType = "arraybuffer";
  bufferedAmount = 0;
  readyState = FakeWebSocket.CONNECTING;
  protocol = L2_TUNNEL_SUBPROTOCOL;

  onopen: (() => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols = protocols;
    FakeWebSocket.last = this;
    FakeWebSocket.instances.push(this);
  }

  open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  send(_data: string | ArrayBuffer | ArrayBufferView | Blob): void {
    // Intentionally empty; tests inspect creation details only.
  }

  close(code?: number, reason?: string): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({ code: code ?? 1000, reason: reason ?? "", wasClean: true } as CloseEvent);
  }
}

function resetFakeWebSocket(): void {
  FakeWebSocket.last = null;
  FakeWebSocket.instances = [];
}

describe("net/connectL2Tunnel", () => {
  it("bootstraps a session with credentials: include", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    const fetchMock = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;
    globalThis.fetch = fetchMock;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
    });

    try {
      expect(fetchMock).toHaveBeenCalledTimes(1);
      const [url, init] = (fetchMock as unknown as { mock: { calls: any[][] } }).mock.calls[0]!;
      expect(url).toBe("https://gateway.example.com/base/session");
      expect(init?.method).toBe("POST");
      expect(init?.credentials).toBe("include");
      expect(init?.headers).toEqual({ "content-type": "application/json" });
      expect(init?.body).toBe("{}");
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("supports explicit wss:// gateway URLs (bootstraps session over https and reuses /l2 path)", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    const fetchMock = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;
    globalThis.fetch = fetchMock;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("wss://gateway.example.com/l2", {
      mode: "ws",
      sink: (ev) => events.push(ev),
    });

    try {
      expect(fetchMock).toHaveBeenCalledTimes(1);
      const [url, init] = (fetchMock as unknown as { mock: { calls: any[][] } }).mock.calls[0]!;
      expect(url).toBe("https://gateway.example.com/session");
      expect(init?.credentials).toBe("include");

      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/l2");
      expect(FakeWebSocket.last?.protocols).toBe(L2_TUNNEL_SUBPROTOCOL);
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("supports same-origin relative gateway base URLs (resolves against location.href)", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;
    const originalLocation = (globalThis as unknown as Record<string, unknown>).location;

    const fetchMock = vi.fn(async () => {
      // Real gateways return `endpoints.l2`; include it so this test exercises
      // the `buildWebSocketUrlFromEndpoint()` path (which must also handle
      // relative gatewayBaseUrl values like "/base").
      return new Response(JSON.stringify({ endpoints: { l2: "/l2" } }), {
        status: 201,
        headers: { "Content-Type": "application/json" },
      });
    }) as unknown as typeof fetch;
    globalThis.fetch = fetchMock;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;
    (globalThis as unknown as Record<string, unknown>).location = { href: "https://gateway.example.com/app/index.html" };

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
    });

    try {
      expect(fetchMock).toHaveBeenCalledTimes(1);
      const [url] = (fetchMock as unknown as { mock: { calls: any[][] } }).mock.calls[0]!;
      expect(url).toBe("https://gateway.example.com/base/session");

      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/base/l2");
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
      if (originalLocation === undefined) delete (globalThis as { location?: unknown }).location;
      else (globalThis as unknown as Record<string, unknown>).location = originalLocation;
    }
  });

  it("ws mode connects to /l2 and requests aero-l2-tunnel-v1 subprotocol", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    globalThis.fetch = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
    });

    try {
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/base/l2");
      expect(FakeWebSocket.last?.protocols).toBe(L2_TUNNEL_SUBPROTOCOL);
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("ws mode supports token auth via Sec-WebSocket-Protocol (tokenTransport=subprotocol)", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    globalThis.fetch = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
      tunnelOptions: { token: "sekrit", tokenTransport: "subprotocol" },
    });

    try {
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/base/l2");
      expect(FakeWebSocket.last?.protocols).toEqual([L2_TUNNEL_SUBPROTOCOL, "aero-l2-token.sekrit"]);
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("honors endpoints.l2 from the gateway session response", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    globalThis.fetch = vi.fn(async () => {
      return new Response(JSON.stringify({ endpoints: { l2: "/l2" } }), {
        status: 201,
        headers: { "Content-Type": "application/json" },
      });
    }) as unknown as typeof fetch;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
    });

    try {
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/base/l2");
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("does not double-prefix endpoints.l2 that already include the gateway base path", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    globalThis.fetch = vi.fn(async () => {
      return new Response(JSON.stringify({ endpoints: { l2: "/base/l2" } }), {
        status: 201,
        headers: { "Content-Type": "application/json" },
      });
    }) as unknown as typeof fetch;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
    });

    try {
      expect(FakeWebSocket.last?.url).toBe("wss://gateway.example.com/base/l2");
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("auto-reconnects on close with exponential backoff", async () => {
    vi.useFakeTimers();
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    const fetchMock = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;
    globalThis.fetch = fetchMock;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
      reconnectBaseDelayMs: 10,
      reconnectMaxDelayMs: 10,
      reconnectJitterFraction: 0,
    });

    try {
      expect(FakeWebSocket.instances.length).toBe(1);
      const first = FakeWebSocket.instances[0]!;

      // Unexpected transport close should trigger a reconnect attempt after the configured delay.
      first.close(1006, "abnormal");
      await vi.advanceTimersByTimeAsync(10);

      expect(fetchMock).toHaveBeenCalledTimes(2);
      expect(FakeWebSocket.instances.length).toBe(2);
      expect(FakeWebSocket.instances[1]!.url).toBe("wss://gateway.example.com/base/l2");
      expect(events.some((e) => e.type === "close")).toBe(true);
    } finally {
      tunnel.close();
      vi.useRealTimers();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("reconnects when the tunnel is idle (no inbound frame/pong)", async () => {
    vi.useFakeTimers();
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    const fetchMock = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;
    globalThis.fetch = fetchMock;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com/base", {
      mode: "ws",
      sink: (ev) => events.push(ev),
      idleTimeoutMs: 10,
      reconnectBaseDelayMs: 10,
      reconnectMaxDelayMs: 10,
      reconnectJitterFraction: 0,
      // Ensure keepalive doesn't create incidental traffic during the test.
      tunnelOptions: { keepaliveMinMs: 60_000, keepaliveMaxMs: 60_000 },
    });

    try {
      expect(FakeWebSocket.instances.length).toBe(1);
      FakeWebSocket.instances[0]!.open();

      // Idle timeout fires at t=10ms, then reconnect backoff fires at t=20ms.
      await vi.advanceTimersByTimeAsync(20);

      expect(fetchMock).toHaveBeenCalledTimes(2);
      expect(FakeWebSocket.instances.length).toBe(2);
      expect(FakeWebSocket.instances[1]!.url).toBe("wss://gateway.example.com/base/l2");
      expect(events.some((e) => e.type === "close")).toBe(true);
    } finally {
      tunnel.close();
      vi.useRealTimers();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });

  it("webrtc mode rejects if the session response does not include udpRelay", async () => {
    const originalFetch = globalThis.fetch;
    globalThis.fetch = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;

    try {
      await expect(
        connectL2Tunnel("https://gateway.example.com", {
          mode: "webrtc",
          sink: () => {},
        }),
      ).rejects.toThrow(/udpRelay/i);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("webrtc mode supports ws(s) UDP relay base URLs from the session response", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalFetch = globalThis.fetch;
    const originalPc = g.RTCPeerConnection;

    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;

    const fetchUrls: string[] = [];
    globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(typeof input === "string" ? input : input.toString());
      fetchUrls.push(url.toString());

      if (url.pathname.endsWith("/session")) {
        return new Response(
          JSON.stringify({
            udpRelay: { baseUrl: "wss://relay.example.com/base", token: "sekrit" },
          }),
          { status: 201, headers: { "Content-Type": "application/json" } },
        );
      }

      if (url.pathname.endsWith("/webrtc/ice")) {
        return new Response(JSON.stringify({ iceServers: [] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }

      if (url.pathname.endsWith("/webrtc/offer")) {
        return new Response(JSON.stringify({ sdp: { type: "answer", sdp: "fake-answer" } }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }

      throw new Error(`unexpected fetch url: ${url.toString()}`);
    }) as unknown as typeof fetch;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com", {
      mode: "webrtc",
      sink: (ev) => events.push(ev),
      relaySignalingMode: "http-offer",
      // Avoid keepalive timers during the test.
      tunnelOptions: { keepaliveMinMs: 0, keepaliveMaxMs: 0 },
    });

    try {
      expect(fetchUrls).toContain("https://gateway.example.com/session");
      expect(fetchUrls).toContain("https://relay.example.com/base/webrtc/ice");
      expect(fetchUrls).toContain("https://relay.example.com/base/webrtc/offer");
      // Guard against regressions where we accidentally call fetch() with a ws(s) URL.
      expect(fetchUrls.some((u) => u.startsWith("ws:") || u.startsWith("wss:"))).toBe(false);
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
    }
  });

  it("does not spam error events (respects L2 tunnel throttling)", async () => {
    const originalFetch = globalThis.fetch;
    const originalWs = (globalThis as unknown as Record<string, unknown>).WebSocket;

    globalThis.fetch = vi.fn(async () => {
      return new Response("{}", { status: 201, headers: { "Content-Type": "application/json" } });
    }) as unknown as typeof fetch;

    resetFakeWebSocket();
    (globalThis as unknown as Record<string, unknown>).WebSocket = FakeWebSocket as unknown as WebSocketConstructor;

    const events: L2TunnelEvent[] = [];
    const tunnel = await connectL2Tunnel("https://gateway.example.com", {
      mode: "ws",
      sink: (ev) => events.push(ev),
      tunnelOptions: { errorIntervalMs: 60_000 },
    });

    try {
      const oversized = new Uint8Array(4096);
      tunnel.sendFrame(oversized);
      tunnel.sendFrame(oversized);
      tunnel.sendFrame(oversized);

      expect(events.filter((e) => e.type === "error").length).toBe(1);
    } finally {
      tunnel.close();
      globalThis.fetch = originalFetch;
      if (originalWs === undefined) delete (globalThis as { WebSocket?: unknown }).WebSocket;
      else (globalThis as unknown as Record<string, unknown>).WebSocket = originalWs;
    }
  });
});
