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
}

let lastSocket: FakeWebSocket | null = null;

describe("WebSocketUdpProxyClient", () => {
  it("sends {type:'auth'} first and waits for {type:'ready'} before sending datagrams", () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalWebSocket = g.WebSocket;
    g.WebSocket = FakeWebSocket;

    try {
      const client = new WebSocketUdpProxyClient("ws://example.com", () => {}, {
        auth: { apiKey: "secret" },
      });
      client.connect();

      expect(lastSocket).not.toBeNull();
      lastSocket!.triggerOpen();

      expect(lastSocket!.sent[0]).toBe(JSON.stringify({ type: "auth", apiKey: "secret" }));
      expect(lastSocket!.sent.length).toBe(1);

      client.send(1234, "127.0.0.1", 53, new Uint8Array([1, 2, 3]));
      expect(lastSocket!.sent.length).toBe(1);

      lastSocket!.triggerMessage(JSON.stringify({ type: "ready" }));
      expect(lastSocket!.sent.length).toBe(2);
      expect(lastSocket!.sent[1]).toBeInstanceOf(Uint8Array);
    } finally {
      if (originalWebSocket === undefined) delete g.WebSocket;
      else g.WebSocket = originalWebSocket;
    }
  });
});
