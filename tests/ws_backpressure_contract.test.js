import assert from "node:assert/strict";
import test from "node:test";

import { createWsSendQueue } from "../src/ws_backpressure.js";

function wait(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function nextImmediate() {
  return new Promise((r) => setImmediate(r));
}

async function waitUntil(predicate, { timeoutMs = 250, intervalMs = 5 } = {}) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return true;
    await wait(intervalMs);
  }
  return predicate();
}

test("ws_backpressure: pauses sources when backlog exceeds high watermark and resumes after drain", async () => {
  const events = [];

  /** @type {{ bufferedAmount: number, send: (data: any, cb?: any) => void, terminate: () => void }} */
  const ws = {
    bufferedAmount: 0,
    send(data, cb) {
      const len = Buffer.isBuffer(data) ? data.byteLength : Buffer.from(data).byteLength;
      this.bufferedAmount += len;
      if (typeof cb === "function") cb(null);
    },
    terminate() {},
  };

  const q = createWsSendQueue({
    ws,
    highWatermarkBytes: 100,
    lowWatermarkBytes: 50,
    pollMs: 10,
    onPauseSources: () => events.push("pause"),
    onResumeSources: () => events.push("resume"),
    onSendError: () => events.push("send_error"),
  });

  q.enqueue(Buffer.alloc(120));
  await nextImmediate();

  assert.equal(events.includes("pause"), true);
  assert.equal(q.isBackpressured(), true);

  // Simulate drain on the underlying ws implementation.
  ws.bufferedAmount = 0;
  const resumed = await waitUntil(() => events.includes("resume"), { timeoutMs: 250, intervalMs: 5 });
  assert.equal(resumed, true);

  assert.equal(events.filter((e) => e === "resume").length, 1);
  assert.equal(q.isBackpressured(), false);
  q.close();
});

test("ws_backpressure: createWsSendQueue tolerates missing opts", async () => {
  const q = createWsSendQueue();
  assert.equal(typeof q.enqueue, "function");
  assert.equal(typeof q.isBackpressured, "function");
  assert.equal(typeof q.backlogBytes, "function");
  assert.equal(typeof q.close, "function");

  q.enqueue(Buffer.from("hello"));
  await nextImmediate();
  assert.equal(q.isBackpressured(), false);
  assert.ok(q.backlogBytes() > 0);

  q.close();
  assert.equal(q.backlogBytes(), 0);
});

test("ws_backpressure: calls onSendError when ws.send throws", async () => {
  const events = [];

  /** @type {{ bufferedAmount: number, send: (data: any, cb?: any) => void, terminate: () => void }} */
  const ws = {
    bufferedAmount: 0,
    send(_data, _cb) {
      throw new Error("boom");
    },
    terminate() {},
  };

  const q = createWsSendQueue({
    ws,
    highWatermarkBytes: 100,
    lowWatermarkBytes: 50,
    pollMs: 10,
    onPauseSources: () => events.push("pause"),
    onResumeSources: () => events.push("resume"),
    onSendError: () => events.push("send_error"),
  });

  q.enqueue(Buffer.from("hi"));
  await nextImmediate();

  assert.equal(events.includes("send_error"), true);
  q.close();
});

test("ws_backpressure: does not throw if bufferedAmount getter throws", async () => {
  const events = [];

  const ws = { send() {}, terminate() {} };
  Object.defineProperty(ws, "bufferedAmount", {
    get() {
      throw new Error("boom");
    },
  });

  const q = createWsSendQueue({
    ws,
    highWatermarkBytes: 1,
    lowWatermarkBytes: 1,
    pollMs: 10,
    onPauseSources: () => events.push("pause"),
    onResumeSources: () => events.push("resume"),
    onSendError: () => events.push("send_error"),
  });

  q.enqueue(Buffer.alloc(2));
  await nextImmediate();

  assert.equal(events.includes("pause"), true);
  q.close();
});

test("ws_backpressure: clamps negative bufferedAmount to 0", () => {
  const ws = { bufferedAmount: -1, send() {}, terminate() {} };
  const q = createWsSendQueue({ ws, highWatermarkBytes: 1, lowWatermarkBytes: 1 });
  assert.equal(q.backlogBytes(), 0);
  q.close();
});

test("ws_backpressure: does not treat primitive ws as open", async () => {
  const events = [];
  // @ts-expect-error - intentionally invalid.
  const q = createWsSendQueue({
    ws: 123,
    highWatermarkBytes: 1,
    lowWatermarkBytes: 1,
    pollMs: 10,
    onPauseSources: () => events.push("pause"),
    onResumeSources: () => events.push("resume"),
    onSendError: () => events.push("send_error"),
  });

  q.enqueue(Buffer.alloc(2));
  await nextImmediate();
  assert.equal(events.length, 0);
  assert.equal(q.isBackpressured(), false);
  q.close();
});

test("ws_backpressure: does not throw if send getter throws", async () => {
  const events = [];
  const ws = {};
  Object.defineProperty(ws, "send", {
    get() {
      throw new Error("boom");
    },
  });

  const q = createWsSendQueue({
    ws,
    highWatermarkBytes: 1,
    lowWatermarkBytes: 1,
    pollMs: 10,
    onPauseSources: () => events.push("pause"),
    onResumeSources: () => events.push("resume"),
    onSendError: () => events.push("send_error"),
  });

  q.enqueue(Buffer.alloc(2));
  await nextImmediate();

  // The main contract is "doesn't throw"; ensure we at least paused.
  assert.equal(events.includes("pause"), true);
  q.close();
});

test("ws_backpressure: does not throw if readyState getter throws", async () => {
  const events = [];
  const ws = { send() {} };
  Object.defineProperty(ws, "readyState", {
    get() {
      throw new Error("boom");
    },
  });

  const q = createWsSendQueue({
    ws,
    highWatermarkBytes: 1,
    lowWatermarkBytes: 1,
    pollMs: 10,
    onPauseSources: () => events.push("pause"),
    onResumeSources: () => events.push("resume"),
    onSendError: () => events.push("send_error"),
  });

  q.enqueue(Buffer.alloc(2));
  await nextImmediate();

  // Treat getter-throw as closed: no pause/resume.
  assert.equal(events.length, 0);
  q.close();
});

test("ws_backpressure: does not throw if OPEN getter throws", async () => {
  const events = [];
  const ws = { readyState: 1, send() {} };
  Object.defineProperty(ws, "OPEN", {
    get() {
      throw new Error("boom");
    },
  });

  const q = createWsSendQueue({
    ws,
    highWatermarkBytes: 1,
    lowWatermarkBytes: 1,
    pollMs: 10,
    onPauseSources: () => events.push("pause"),
    onResumeSources: () => events.push("resume"),
    onSendError: () => events.push("send_error"),
  });

  q.enqueue(Buffer.alloc(2));
  await nextImmediate();

  assert.equal(events.length, 0);
  q.close();
});

