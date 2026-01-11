import { describe, expect, it, vi } from "vitest";

import { connectL2RelaySignaling } from "./l2RelaySignalingClient";

type Listener = (evt: Event) => void;

class FakeRtcDataChannel {
  label = "";
  readyState: RTCDataChannelState;
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
  localDescription: RTCSessionDescriptionInit | null = null;

  createdLabel: string | null = null;
  createdInit: RTCDataChannelInit | undefined = undefined;
  createdChannel: FakeRtcDataChannel | null = null;

  closed = false;

  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  constructor(_config: RTCConfiguration) {
    FakePeerConnection.last = this;
  }

  createDataChannel(label: string, init?: RTCDataChannelInit): RTCDataChannel {
    this.createdLabel = label;
    this.createdInit = init;
    const channel = FakePeerConnection.nextDataChannel ?? new FakeRtcDataChannel("open");
    FakePeerConnection.nextDataChannel = null;
    channel.label = label;
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
  it("creates an l2 DataChannel with reliable/unordered semantics", async () => {
    const g = globalThis as unknown as Record<string, unknown>;
    const originalPc = g.RTCPeerConnection;
    g.RTCPeerConnection = FakePeerConnection as unknown as typeof RTCPeerConnection;
    const restoreFetch = installMockFetch();

    FakePeerConnection.last = null;
    FakePeerConnection.nextDataChannel = new FakeRtcDataChannel("open");

    try {
      await connectL2RelaySignaling({ baseUrl: "https://relay.example.com", mode: "http-offer" });
      const pc = FakePeerConnection.last;
      expect(pc).not.toBeNull();

      expect(pc!.createdLabel).toBe("l2");
      expect(pc!.createdInit).toBeDefined();
      expect(pc!.createdInit?.ordered).toBe(false);
      expect(pc!.createdInit?.maxRetransmits).toBeUndefined();
      expect(pc!.createdInit?.maxPacketLifeTime).toBeUndefined();
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

    FakePeerConnection.last = null;
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

    FakePeerConnection.last = null;
    const dc = new FakeRtcDataChannel("connecting");
    FakePeerConnection.nextDataChannel = dc;

    try {
      const promise = connectL2RelaySignaling({ baseUrl: "https://relay.example.com", mode: "http-offer" });
      await dc.openListenerAdded;
      dc.emitError();

      await expect(promise).rejects.toThrow("data channel error");
      expect(FakePeerConnection.last?.closed).toBe(true);
    } finally {
      restoreFetch();
      if (originalPc === undefined) delete g.RTCPeerConnection;
      else g.RTCPeerConnection = originalPc;
    }
  });
});
