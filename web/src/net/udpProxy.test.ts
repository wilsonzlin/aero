import { describe, expect, it } from "vitest";
import { WebSocketUdpProxyClient } from "./udpProxy";

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

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
    lastSocket = this;
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

  triggerError(): void {
    this.onerror?.({} as Event);
  }
}

let lastSocket: FakeWebSocket | null = null;

describe("WebSocketUdpProxyClient", () => {
  it("sends {type:'auth'} first and waits for {type:'ready'} before sending datagrams", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketUdpProxyClient("ws://example.com", () => {}, {
        auth: { apiKey: "secret" },
      });
      const connectPromise = client.connect();

      expect(lastSocket).not.toBeNull();
      expect(lastSocket!.url).toBe("ws://example.com/udp");
      lastSocket!.triggerOpen();

      expect(lastSocket!.sent[0]).toBe(JSON.stringify({ type: "auth", token: "secret", apiKey: "secret" }));
      expect(lastSocket!.sent.length).toBe(1);

      client.send(1234, "127.0.0.1", 53, new Uint8Array([1, 2, 3]));
      expect(lastSocket!.sent.length).toBe(1);

      lastSocket!.triggerMessage(JSON.stringify({ type: "ready" }));
      await connectPromise;
      expect(lastSocket!.sent.length).toBe(2);
      expect(lastSocket!.sent[1]).toBeInstanceOf(Uint8Array);
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });

  it("normalizes https:// base URLs to wss:// and appends /udp", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketUdpProxyClient("https://example.com/base", () => {});
      const connectPromise = client.connect();

      expect(lastSocket).not.toBeNull();
      expect(lastSocket!.url).toBe("wss://example.com/base/udp");

      lastSocket!.triggerOpen();
      lastSocket!.triggerMessage(JSON.stringify({ type: "ready" }));
      await connectPromise;
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
      const client = new WebSocketUdpProxyClient("wss://example.com/base", () => {});
      const connectPromise = client.connect();

      expect(lastSocket).not.toBeNull();
      expect(lastSocket!.url).toBe("wss://example.com/base/udp");

      lastSocket!.triggerOpen();
      lastSocket!.triggerMessage(JSON.stringify({ type: "ready" }));
      await connectPromise;
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
    (g as { location?: unknown }).location = { href: "https://example.com/app/index.html" };

    try {
      const client = new WebSocketUdpProxyClient("/gw", () => {});
      const connectPromise = client.connect();

      expect(lastSocket).not.toBeNull();
      expect(lastSocket!.url).toBe("wss://example.com/gw/udp");

      lastSocket!.triggerOpen();
      lastSocket!.triggerMessage(JSON.stringify({ type: "ready" }));
      await connectPromise;
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
      if (originalLocation === undefined) delete (g as { location?: unknown }).location;
      else (g as { location?: unknown }).location = originalLocation;
    }
  });
});
