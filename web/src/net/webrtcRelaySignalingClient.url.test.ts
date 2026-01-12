import { describe, expect, it } from "vitest";

import { toHttpUrl, toWebSocketUrl } from "./webrtcRelaySignalingClient";

describe("net/webrtcRelaySignalingClient URL normalization", () => {
  it("maps wss:// base URLs to https:// for HTTP endpoints and preserves wss:// for WebSocket endpoints", () => {
    const baseUrl = "wss://relay.example.com/base";

    expect(toHttpUrl(baseUrl, "/webrtc/ice").toString()).toBe("https://relay.example.com/base/webrtc/ice");
    expect(toWebSocketUrl(baseUrl, "/webrtc/signal").toString()).toBe("wss://relay.example.com/base/webrtc/signal");
  });

  it("maps ws:// base URLs to http:// for HTTP endpoints and keeps ws:// for WebSocket endpoints", () => {
    const baseUrl = "ws://relay.example.com";

    expect(toHttpUrl(baseUrl, "/webrtc/ice").toString()).toBe("http://relay.example.com/webrtc/ice");
    expect(toWebSocketUrl(baseUrl, "/webrtc/signal").toString()).toBe("ws://relay.example.com/webrtc/signal");
  });

  it("maps https:// base URLs to wss:// for WebSocket endpoints and appends paths without double slashes", () => {
    const baseUrl = "https://relay.example.com/base/";

    expect(toWebSocketUrl(baseUrl, "/webrtc/signal").toString()).toBe("wss://relay.example.com/base/webrtc/signal");
  });
});

