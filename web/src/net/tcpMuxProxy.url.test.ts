import { describe, expect, it } from "vitest";
import { WebSocketTcpMuxProxyClient } from "./tcpMuxProxy";

type Listener = (ev: Event) => void;

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  static last: FakeWebSocket | null = null;

  readonly url: string;
  readonly protocols?: string | string[];
  readyState = FakeWebSocket.CONNECTING;
  binaryType: BinaryType = "blob";
  bufferedAmount = 0;
  protocol = "";

  onopen: (() => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;

  private readonly closeListeners: Array<{ listener: Listener; once: boolean }> = [];

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols = protocols;
    FakeWebSocket.last = this;
  }

  send(): void {}

  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({} as CloseEvent);
    const listeners = [...this.closeListeners];
    this.closeListeners.length = 0;
    for (const { listener } of listeners) listener(new Event("close"));
  }

  addEventListener(type: string, listener: Listener, opts?: AddEventListenerOptions | boolean): void {
    if (type !== "close") return;
    const once = typeof opts === "object" && opts !== null && (opts as AddEventListenerOptions).once === true;
    this.closeListeners.push({ listener, once });
  }
}

describe("WebSocketTcpMuxProxyClient URL normalization", () => {
  it("normalizes https:// base URLs to wss:// and appends /tcp-mux", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpMuxProxyClient("https://gateway.example.com/base");
      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toBe("wss://gateway.example.com/base/tcp-mux");
      await client.shutdown();
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("avoids duplicate slashes when base URL ends with '/'", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpMuxProxyClient("https://gateway.example.com/base/");
      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toBe("wss://gateway.example.com/base/tcp-mux");
      await client.shutdown();
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("normalizes http:// base URLs to ws:// and appends /tcp-mux", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpMuxProxyClient("http://gateway.example.com/base");
      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toBe("ws://gateway.example.com/base/tcp-mux");
      await client.shutdown();
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("keeps ws:// scheme when provided explicitly", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpMuxProxyClient("ws://gateway.example.com/base");
      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toBe("ws://gateway.example.com/base/tcp-mux");
      await client.shutdown();
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("keeps wss:// scheme when provided explicitly", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpMuxProxyClient("wss://gateway.example.com/base");
      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toBe("wss://gateway.example.com/base/tcp-mux");
      await client.shutdown();
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("resolves same-origin /path base URLs against location.href when available", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    const originalLocation = (g as { location?: unknown }).location;

    g.WebSocket = FakeWebSocket;
    (g as { location?: unknown }).location = { href: "https://gateway.example.com/app/index.html" };

    try {
      const client = new WebSocketTcpMuxProxyClient("/base");
      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toBe("wss://gateway.example.com/base/tcp-mux");
      await client.shutdown();
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
      if (originalLocation === undefined) delete (g as { location?: unknown }).location;
      else (g as { location?: unknown }).location = originalLocation;
    }
  });
});
