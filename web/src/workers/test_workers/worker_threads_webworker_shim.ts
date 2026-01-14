import { parentPort } from "node:worker_threads";

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

// Minimal Web Worker messaging facade for Node `worker_threads`, so production worker entrypoints
// (which expect `self`, `postMessage`, `onmessage`, and `addEventListener("message")`) can run
// under unit tests.
(globalThis as unknown as { self?: unknown }).self = globalThis;
(globalThis as unknown as { postMessage?: unknown }).postMessage = (msg: unknown, transfer?: Transferable[]) =>
  (parentPort as unknown as { postMessage: (msg: unknown, transferList?: unknown[]) => void } | null)?.postMessage(
    msg,
    transfer as unknown as unknown[],
  );
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

