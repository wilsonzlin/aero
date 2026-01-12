import { describe, expect, it, vi } from "vitest";

import { connectRelaySignaling } from "./webrtcRelaySignalingClient";

type Listener = (evt: Event) => void;

class FakePeerConnection {
  static last: FakePeerConnection | null = null;

  iceGatheringState: RTCIceGatheringState = "complete";
  connectionState: RTCPeerConnectionState = "new";

  localDescription: RTCSessionDescriptionInit | null = null;
  remoteDescription: RTCSessionDescriptionInit | null = null;

  private readonly listeners = new Map<string, Set<Listener>>();

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  constructor(_config: RTCConfiguration) {
    FakePeerConnection.last = this;
  }

  addEventListener(type: string, listener: Listener): void {
    let set = this.listeners.get(type);
    if (!set) {
      set = new Set();
      this.listeners.set(type, set);
    }
    set.add(listener);
  }

  removeEventListener(type: string, listener: Listener): void {
    this.listeners.get(type)?.delete(listener);
  }

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  async addIceCandidate(_candidate: RTCIceCandidateInit): Promise<void> {}

  async createOffer(): Promise<RTCSessionDescriptionInit> {
    return { type: "offer", sdp: "fake-offer" };
  }

  async setLocalDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.localDescription = desc;
  }

  async setRemoteDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.remoteDescription = desc;
  }

  close(): void {
    this.connectionState = "closed";
  }
}

type WsListener = (evt: Event | MessageEvent) => void;

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  static last: FakeWebSocket | null = null;

  readonly url: string;
  readonly protocols?: string | string[];

  readyState = FakeWebSocket.CONNECTING;
  readonly sent: string[] = [];

  private readonly listeners = new Map<string, Set<WsListener>>();

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols = protocols;
    FakeWebSocket.last = this;

    // Defer open until after `openWebSocket()` has installed event listeners.
    queueMicrotask(() => {
      this.readyState = FakeWebSocket.OPEN;
      this.dispatch("open", new Event("open"));
    });
  }

  addEventListener(type: string, listener: WsListener): void {
    let set = this.listeners.get(type);
    if (!set) {
      set = new Set();
      this.listeners.set(type, set);
    }
    set.add(listener);
  }

  removeEventListener(type: string, listener: WsListener): void {
    this.listeners.get(type)?.delete(listener);
  }

  send(data: string): void {
    this.sent.push(data);

    // Minimal fake server: respond to the client's offer immediately.
    try {
      const msg = JSON.parse(data) as { type?: string };
      if (msg.type === "offer") {
        queueMicrotask(() => {
          this.dispatch(
            "message",
            {
              data: JSON.stringify({ type: "answer", sdp: { type: "answer", sdp: "fake-answer" } }),
            } as MessageEvent,
          );
        });
      }
    } catch {
      // Ignore non-JSON payloads.
    }
  }

  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.dispatch("close", { code: 1000, reason: "", wasClean: true } as CloseEvent);
  }

  private dispatch(type: string, evt: Event | MessageEvent): void {
    for (const listener of this.listeners.get(type) ?? []) {
      listener(evt);
    }
  }
}

function installMockFetch(handler: (url: URL) => Response | Promise<Response>): () => void {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(typeof input === "string" ? input : input.toString());
    return await handler(url);
  }) as unknown as typeof fetch;
  return () => {
    globalThis.fetch = originalFetch;
  };
}

describe("net/webrtcRelaySignalingClient", () => {
  it("uses https:// for HTTP endpoints when baseUrl is wss://", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;

    const fetchUrls: string[] = [];
    const restoreFetch = installMockFetch(async (url) => {
      fetchUrls.push(url.toString());
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
    });

    try {
      await connectRelaySignaling({ baseUrl: "wss://relay.example.com/base", mode: "http-offer" }, () => {
        return { readyState: "open" } as RTCDataChannel;
      });

      expect(fetchUrls).toContain("https://relay.example.com/base/webrtc/ice");
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
    }
  });

  it("uses http:// for HTTP endpoints when baseUrl is ws://", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;

    const fetchUrls: string[] = [];
    const restoreFetch = installMockFetch(async (url) => {
      fetchUrls.push(url.toString());
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
    });

    try {
      await connectRelaySignaling({ baseUrl: "ws://relay.example.com/base", mode: "http-offer" }, () => {
        return { readyState: "open" } as RTCDataChannel;
      });

      expect(fetchUrls).toContain("http://relay.example.com/base/webrtc/ice");
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
    }
  });

  it("preserves wss:// and base path for signaling WebSockets", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    const originalWs = g.WebSocket;
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;
    g.WebSocket = FakeWebSocket as unknown as typeof WebSocket;

    const restoreFetch = installMockFetch(async (url) => {
      if (url.pathname.endsWith("/webrtc/ice")) {
        return new Response(JSON.stringify({ iceServers: [] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
      throw new Error(`unexpected fetch url: ${url.toString()}`);
    });

    try {
      FakeWebSocket.last = null;

      await connectRelaySignaling({ baseUrl: "wss://relay.example.com/base" }, () => {
        return { readyState: "open" } as RTCDataChannel;
      });

      expect(FakeWebSocket.last?.url).toBe("wss://relay.example.com/base/webrtc/signal");
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
      if (originalWs === undefined) delete g.WebSocket;
      else g.WebSocket = originalWs;
    }
  });
});

