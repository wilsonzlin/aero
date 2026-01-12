import { describe, expect, it, vi } from "vitest";

import { connectL2RelaySignaling } from "./l2RelaySignalingClient";

type Listener = (evt: Event) => void;
type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocket;

class FakeRtcDataChannel {
  label = "";
  readyState: RTCDataChannelState;
  ordered = true;
  maxRetransmits: number | null = null;
  maxPacketLifeTime: number | null = null;

  private readonly listeners = new Map<string, Set<Listener>>();

  private resolveOpenListenerAdded: (() => void) | null = null;
  readonly openListenerAdded: Promise<void>;

  constructor(state: RTCDataChannelState) {
    this.readyState = state;
    this.openListenerAdded = new Promise((resolve) => {
      this.resolveOpenListenerAdded = resolve;
    });
  }

  addEventListener(type: string, listener: Listener): void {
    if (type === "open" && this.resolveOpenListenerAdded) {
      this.resolveOpenListenerAdded();
      this.resolveOpenListenerAdded = null;
    }
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

  close(): void {
    this.readyState = "closed";
    this.dispatch("close");
  }

  emitOpen(): void {
    this.readyState = "open";
    this.dispatch("open");
  }

  emitError(): void {
    this.dispatch("error");
  }

  private dispatch(type: string): void {
    const evt = new Event(type);
    for (const listener of this.listeners.get(type) ?? []) {
      listener(evt);
    }
  }
}

class FakePeerConnection {
  static last: FakePeerConnection | null = null;
  static nextDataChannel: FakeRtcDataChannel | null = null;

  iceGatheringState: RTCIceGatheringState = "complete";
  connectionState: RTCPeerConnectionState = "new";
  localDescription: RTCSessionDescriptionInit | null = null;

  createdLabel: string | null = null;
  createdInit: RTCDataChannelInit | undefined = undefined;
  createdChannel: FakeRtcDataChannel | null = null;

  closed = false;

  private readonly listeners = new Map<string, Set<(evt: Event) => void>>();

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  constructor(_config: RTCConfiguration) {
    FakePeerConnection.last = this;
  }

  addEventListener(type: string, listener: (evt: Event) => void): void {
    let set = this.listeners.get(type);
    if (!set) {
      set = new Set();
      this.listeners.set(type, set);
    }
    set.add(listener);
  }

  removeEventListener(type: string, listener: (evt: Event) => void): void {
    this.listeners.get(type)?.delete(listener);
  }

  createDataChannel(label: string, init?: RTCDataChannelInit): RTCDataChannel {
    this.createdLabel = label;
    this.createdInit = init;
    const channel = FakePeerConnection.nextDataChannel ?? new FakeRtcDataChannel("open");
    FakePeerConnection.nextDataChannel = null;
    channel.label = label;
    channel.ordered = init?.ordered ?? true;
    channel.maxRetransmits = init?.maxRetransmits ?? null;
    channel.maxPacketLifeTime = init?.maxPacketLifeTime ?? null;
    this.createdChannel = channel;
    return channel as unknown as RTCDataChannel;
  }

  async createOffer(): Promise<RTCSessionDescriptionInit> {
    return { type: "offer", sdp: "fake-offer" };
  }

  async setLocalDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.localDescription = desc;
  }

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  async setRemoteDescription(_desc: RTCSessionDescriptionInit): Promise<void> {}

  close(): void {
    this.closed = true;
    this.connectionState = "closed";
  }
}

function resetFakePeerConnection(): void {
  FakePeerConnection.last = null;
  FakePeerConnection.nextDataChannel = null;
}

class ThrowingWebSocket {
  static urls: string[] = [];

  constructor(url: string) {
    ThrowingWebSocket.urls.push(url);
    throw new Error("websocket failed");
  }
}

function installMockFetch(): () => void {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(typeof input === "string" ? input : input.toString());

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

  return () => {
    globalThis.fetch = originalFetch;
  };
}

describe("net/l2RelaySignalingClient", () => {
  it("creates an l2 DataChannel with reliable semantics (ordered)", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;
    const restoreFetch = installMockFetch();

    resetFakePeerConnection();
    FakePeerConnection.nextDataChannel = new FakeRtcDataChannel("open");

    try {
      await connectL2RelaySignaling({ baseUrl: "https://relay.example.com", mode: "http-offer" });
      const pc = FakePeerConnection.last;
      if (!pc) throw new Error("expected peer connection to be created");

      expect(pc.createdLabel).toBe("l2");
      expect(pc.createdInit).toBeDefined();
      expect(pc.createdInit?.ordered).toBe(true);
      expect(pc.createdInit?.maxRetransmits).toBeUndefined();
      expect(pc.createdInit?.maxPacketLifeTime).toBeUndefined();
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
    }
  });

  it("waits for the DataChannel to open", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;
    const restoreFetch = installMockFetch();

    resetFakePeerConnection();
    const dc = new FakeRtcDataChannel("connecting");
    FakePeerConnection.nextDataChannel = dc;

    try {
      let settled = false;
      const promise = connectL2RelaySignaling({ baseUrl: "https://relay.example.com", mode: "http-offer" }).finally(() => {
        settled = true;
      });

      await dc.openListenerAdded;
      expect(settled).toBe(false);

      dc.emitOpen();
      await promise;
      expect(settled).toBe(true);
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
    }
  });

  it("closes the PeerConnection when the DataChannel errors before opening", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;
    const restoreFetch = installMockFetch();

    resetFakePeerConnection();
    const dc = new FakeRtcDataChannel("connecting");
    FakePeerConnection.nextDataChannel = dc;

    try {
      const promise = connectL2RelaySignaling({ baseUrl: "https://relay.example.com", mode: "http-offer" });
      await dc.openListenerAdded;
      dc.emitError();

      await expect(promise).rejects.toThrow("data channel error");
      const pc = FakePeerConnection.last;
      if (!pc) throw new Error("expected peer connection to be created");
      expect(pc.closed).toBe(true);
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
    }
  });

  it("normalizes wss:// relay base URLs (fetch over https and signal over wss)", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    const originalWs = g.WebSocket;
    const restoreFetch = installMockFetch();

    resetFakePeerConnection();
    FakePeerConnection.nextDataChannel = new FakeRtcDataChannel("open");
    ThrowingWebSocket.urls = [];
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;
    g.WebSocket = ThrowingWebSocket as unknown as WebSocketConstructor;

    try {
      await expect(connectL2RelaySignaling({ baseUrl: "wss://relay.example.com" })).rejects.toThrow(/websocket/i);

      const fetchMock = globalThis.fetch as unknown as { mock: { calls: any[][] } };
      expect(fetchMock.mock.calls.length).toBeGreaterThanOrEqual(1);
      const [firstUrl] = fetchMock.mock.calls[0]!;
      expect(typeof firstUrl).toBe("string");
      expect(firstUrl).toBe("https://relay.example.com/webrtc/ice");

      expect(ThrowingWebSocket.urls[0]).toBe("wss://relay.example.com/webrtc/signal");
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
      if (originalWs === undefined) delete g.WebSocket;
      else g.WebSocket = originalWs;
    }
  });
});
