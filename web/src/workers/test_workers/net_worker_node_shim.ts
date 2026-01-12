import { parentPort } from "node:worker_threads";

import { L2_TUNNEL_SUBPROTOCOL } from "../../net/l2Tunnel";
import {
  L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
} from "../../shared/l2TunnelProtocol";

type MessageListener = (event: { data: unknown }) => void;

const messageListeners = new Set<MessageListener>();

let messageSeq = 0;

function postToParent(msg: unknown): void {
  parentPort?.postMessage({ ...(msg as Record<string, unknown>), seq: ++messageSeq });
}

function dispatchMessage(data: unknown): void {
  const event = { data };
  const g = globalThis as unknown as { onmessage?: ((ev: { data: unknown }) => void) | null };
  g.onmessage?.(event);
  for (const listener of messageListeners) {
    listener(event);
  }
}

// Minimal Web Worker messaging facade for Node `worker_threads`, so that the
// production worker entrypoints (which expect `postMessage`, `onmessage`, and
// `addEventListener("message")`) can run under tests.
(globalThis as unknown as { self?: unknown }).self = globalThis;
(globalThis as unknown as { postMessage?: unknown }).postMessage = (msg: unknown, transfer?: Transferable[]) =>
  (parentPort as unknown as { postMessage: (msg: unknown, transferList?: unknown[]) => void } | null)?.postMessage(msg, transfer as unknown as unknown[]);
(globalThis as unknown as { close?: unknown }).close = () => parentPort?.close();

// Provide a stable location.href so the worker code can resolve same-origin
// `/path` proxyUrl values via `new URL(path, location.href)`.
(globalThis as unknown as { location?: unknown }).location = { href: "https://gateway.example.com/app/index.html" };

(globalThis as unknown as { addEventListener?: unknown }).addEventListener = (type: string, listener: MessageListener) => {
  if (type !== "message") return;
  messageListeners.add(listener);
};

(globalThis as unknown as { removeEventListener?: unknown }).removeEventListener = (type: string, listener: MessageListener) => {
  if (type !== "message") return;
  messageListeners.delete(listener);
};

parentPort?.on("message", (data) => {
  dispatchMessage(data);
});

// ---------------------------------------------------------------------------
// Fake WebSocket for worker_threads tests.
// ---------------------------------------------------------------------------

type WebSocketSentMessage = { type: "ws.sent"; data: Uint8Array };
type WebSocketCreatedMessage = { type: "ws.created"; url: string; protocol: string };

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
  protocol = "";

  onopen: (() => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols = protocols;
    this.protocol = typeof protocols === "string" ? protocols : Array.isArray(protocols) ? protocols[0] ?? "" : "";
    FakeWebSocket.last = this;

    postToParent({
      type: "ws.created",
      url,
      protocol: this.protocol,
    } satisfies WebSocketCreatedMessage);

    // Auto-open on the next microtask so `WebSocketL2TunnelClient` sees an async open.
    queueMicrotask(() => this.open());
  }

  private open(): void {
    if (this.readyState !== FakeWebSocket.CONNECTING) return;
    // Enforce the expected L2 subprotocol so the client doesn't immediately close.
    if (this.protocol !== L2_TUNNEL_SUBPROTOCOL) {
      this.protocol = "";
    }
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  send(data: string | ArrayBuffer | ArrayBufferView | Blob): void {
    if (typeof data === "string" || data instanceof Blob) {
      throw new Error("FakeWebSocket.send: unexpected payload type");
    }

    const view =
      data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);

    // Copy to avoid aliasing.
    const copy = view.slice();
    postToParent({ type: "ws.sent", data: copy } satisfies WebSocketSentMessage);
  }

  close(code?: number, reason?: string): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({ code: code ?? 1000, reason: reason ?? "", wasClean: true } as CloseEvent);
  }

  emitMessage(payload: Uint8Array): void {
    const buf = payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength);
    this.onmessage?.({ data: buf } as MessageEvent);
  }
}

(globalThis as unknown as { WebSocket?: unknown }).WebSocket = FakeWebSocket as unknown as typeof WebSocket;

parentPort?.on("message", (msg) => {
  const data = msg as { type?: unknown; data?: unknown };
  if (data?.type !== "ws.inject") return;
  if (!(data.data instanceof Uint8Array)) return;
  FakeWebSocket.last?.emitMessage(data.data);
});

parentPort?.on("message", (msg) => {
  const data = msg as { type?: unknown; code?: unknown; reason?: unknown };
  if (data?.type !== "ws.close") return;
  const code = typeof data.code === "number" ? data.code : undefined;
  const reason = typeof data.reason === "string" ? data.reason : undefined;
  FakeWebSocket.last?.close(code, reason);
});

// ---------------------------------------------------------------------------
// Fake fetch for worker_threads tests (gateway session bootstrap).
// ---------------------------------------------------------------------------

type FetchMode = "ok" | "404" | "throw" | "bad_json";
let fetchMode: FetchMode = "ok";

parentPort?.on("message", (msg) => {
  const data = msg as { type?: unknown; mode?: unknown };
  if (data?.type !== "fetch.mode") return;
  if (data.mode === "ok" || data.mode === "404" || data.mode === "throw" || data.mode === "bad_json") {
    fetchMode = data.mode;
  }
});

type FakeFetchCalledMessage = { type: "fetch.called"; url: string; init?: unknown };

function normalizeFetchUrl(input: unknown): string {
  if (typeof input === "string") return input;
  if (input instanceof URL) return input.toString();
  if (typeof input === "object" && input !== null && "url" in input && typeof (input as any).url === "string") {
    return (input as any).url;
  }
  return String(input);
}

function makeFakeResponse(args: { ok: boolean; status: number; bodyText: string; bodyJson?: unknown }): Response {
  // Use the built-in Node Response when available (Node 18+), otherwise fall back
  // to a minimal shim object.
  const Res = (globalThis as unknown as { Response?: typeof Response }).Response;
  if (typeof Res === "function") {
    return new Res(args.bodyText, {
      status: args.status,
      headers: { "content-type": "application/json" },
    });
  }
  return {
    ok: args.ok,
    status: args.status,
    async json() {
      if (args.bodyJson !== undefined) return args.bodyJson;
      return JSON.parse(args.bodyText);
    },
    async text() {
      return args.bodyText;
    },
  } as unknown as Response;
}

(globalThis as unknown as { fetch?: unknown }).fetch = async (input: unknown, init?: RequestInit): Promise<Response> => {
  const url = normalizeFetchUrl(input);
  const method = typeof init?.method === "string" ? init.method : undefined;
  postToParent({ type: "fetch.called", url, init } satisfies FakeFetchCalledMessage);

  if (fetchMode === "throw") {
    throw new Error("Fake fetch: simulated network error");
  }

  // Only allow the gateway session bootstrap URLs used by the unit tests.
  const allowedUrls = new Set(["https://gateway.example.com/session", "https://gateway.example.com/base/session"]);

  if (!allowedUrls.has(url) || method !== "POST") {
    return makeFakeResponse({ ok: false, status: 404, bodyText: "not found" });
  }

  if (fetchMode === "404") {
    return makeFakeResponse({ ok: false, status: 404, bodyText: "not found" });
  }

  if (fetchMode === "bad_json") {
    return makeFakeResponse({ ok: true, status: 201, bodyText: "not-json" });
  }

  const body = {
    endpoints: { l2: "/l2" },
    limits: {
      l2: {
        maxFramePayloadBytes: L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
        maxControlPayloadBytes: L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
      },
    },
  };
  return makeFakeResponse({ ok: true, status: 201, bodyText: JSON.stringify(body), bodyJson: body });
};
