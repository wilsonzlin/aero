import { describe, expect, it, vi } from "vitest";

import { L2_TUNNEL_SUBPROTOCOL, type L2TunnelEvent } from "./l2Tunnel";
import { connectL2Tunnel } from "./connectL2Tunnel";

type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocket;

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  static last: FakeWebSocket | null = null;

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
