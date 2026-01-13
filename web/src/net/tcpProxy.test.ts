import { describe, expect, it } from "vitest";
import { type TcpProxyEvent, WebSocketTcpProxyClient } from "./tcpProxy";

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  static readonly instances: FakeWebSocket[] = [];
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
    FakeWebSocket.instances.push(this);
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
      const client = new WebSocketTcpProxyClient("https://gateway.example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("gateway.example.com");
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
      const client = new WebSocketTcpProxyClient("https://gateway.example.com/base/", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("gateway.example.com");
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
      const client = new WebSocketTcpProxyClient("http://gateway.example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("ws:");
      expect(url.hostname).toBe("gateway.example.com");
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
      const client = new WebSocketTcpProxyClient("ws://gateway.example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("ws:");
      expect(url.hostname).toBe("gateway.example.com");
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
      const client = new WebSocketTcpProxyClient("wss://gateway.example.com/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("gateway.example.com");
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
    (g as { location?: unknown }).location = { href: "https://gateway.example.com/app/index.html" };

    try {
      const client = new WebSocketTcpProxyClient("/base", () => {});
      client.connect(1, "127.0.0.1", 1234);

      expect(FakeWebSocket.last).not.toBeNull();
      expect(FakeWebSocket.last!.url).toContain("wss://gateway.example.com/base/tcp");
      const url = new URL(FakeWebSocket.last!.url);
      expect(url.protocol).toBe("wss:");
      expect(url.hostname).toBe("gateway.example.com");
      expect(url.pathname).toBe("/base/tcp");
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
      if (originalLocation === undefined) delete (g as { location?: unknown }).location;
      else (g as { location?: unknown }).location = originalLocation;
    }
  });
});

describe("WebSocketTcpProxyClient lifecycle", () => {
  it("removes sockets from the internal map on remote close and allows reconnect", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      // Reset global FakeWebSocket state for this test.
      FakeWebSocket.instances.length = 0;
      FakeWebSocket.last = null;

      const events: TcpProxyEvent[] = [];
      const client = new WebSocketTcpProxyClient("https://gateway.example.com/base", (evt) => events.push(evt));

      client.connect(1, "127.0.0.1", 1234);
      // Keep an explicit union type here: TypeScript may treat `FakeWebSocket.last` as definitely
      // null after we reset it above, since it cannot see the side-effecting assignment from
      // `client.connect()` into our FakeWebSocket constructor.
      const first: FakeWebSocket | null = FakeWebSocket.last;
      expect(first).not.toBeNull();

      // Simulate remote close (i.e. without calling client.close()).
      first!.onclose?.({} as CloseEvent);

      // Should be able to reconnect using the same connection ID.
      client.connect(1, "127.0.0.1", 1234);
      const second: FakeWebSocket | null = FakeWebSocket.last;
      expect(second).not.toBeNull();
      expect(second).not.toBe(first);

      // Ensure URL is the same for both sockets (and still correct).
      expect(new URL(second!.url).toString()).toBe(new URL(first!.url).toString());
      expect(new URL(second!.url).pathname).toBe("/base/tcp");

      expect(events).toContainEqual({ type: "closed", connectionId: 1 });
      expect(FakeWebSocket.instances).toHaveLength(2);
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });
});
