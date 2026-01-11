import { parentPort } from "node:worker_threads";

import { L2_TUNNEL_SUBPROTOCOL } from "../../net/l2Tunnel";

type MessageListener = (event: { data: unknown }) => void;

const messageListeners = new Set<MessageListener>();

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
(globalThis as unknown as { postMessage?: unknown }).postMessage = (msg: unknown) => parentPort?.postMessage(msg);
(globalThis as unknown as { close?: unknown }).close = () => parentPort?.close();

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
    parentPort?.postMessage({ type: "ws.sent", data: copy } satisfies WebSocketSentMessage);
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
