import { describe, expect, it } from "vitest";
import { WebSocketTcpProxyClient } from "./tcpProxy";

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

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.last = this;
  }

  send(): void {}

  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({} as CloseEvent);
  }
}

describe("WebSocketTcpProxyClient URL normalization", () => {
  it("normalizes https:// base URLs to wss:// and appends /tcp", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpProxyClient("https://example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("example.com");
      expect(url.pathname).toBe("/base/tcp");
      expect(url.searchParams.get("v")).toBe("1");
      expect(url.searchParams.get("host")).toBe("127.0.0.1");
      expect(url.searchParams.get("port")).toBe("1234");
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("avoids duplicate slashes when base URL ends with '/'", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpProxyClient("https://example.com/base/", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("example.com");
      expect(url.pathname).toBe("/base/tcp");
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("normalizes http:// base URLs to ws:// and appends /tcp", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpProxyClient("http://example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("ws:");
      expect(url.hostname).toBe("example.com");
      expect(url.pathname).toBe("/base/tcp");
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("keeps ws:// scheme when provided explicitly", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpProxyClient("ws://example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("ws:");
      expect(url.hostname).toBe("example.com");
      expect(url.pathname).toBe("/base/tcp");
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("keeps wss:// scheme when provided explicitly", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketTcpProxyClient("wss://example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("example.com");
      expect(url.pathname).toBe("/base/tcp");
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("resolves same-origin /path base URLs against location.href when available", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    const originalLocation = (g as { location?: unknown }).location;

    g.WebSocket = FakeWebSocket;
    (g as { location?: unknown }).location = { href: "https://example.com/app/index.html" };

    try {
      const client = new WebSocketTcpProxyClient("/gw", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toContain("wss://example.com/gw/tcp");
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("example.com");
      expect(url.pathname).toBe("/gw/tcp");
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
      if (originalLocation === undefined) delete (g as { location?: unknown }).location;
      else (g as { location?: unknown }).location = originalLocation;
    }
  });
});
