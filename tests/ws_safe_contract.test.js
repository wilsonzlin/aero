import test from "node:test";
import assert from "node:assert/strict";

import { wsCloseSafe, wsIsOpenSafe, wsSendSafe } from "../scripts/_shared/ws_safe.js";

test("ws_safe: wsSendSafe does not pass callback to 1-arg send()", async () => {
  let cbErr;
  let cbCalled = false;

  const ws = {
    OPEN: 1,
    readyState: 1,
    send: function (data) {
      assert.equal(data, "hello");
      assert.equal(arguments.length, 1);
    },
  };

  const ok = wsSendSafe(ws, "hello", (err) => {
    cbErr = err;
    cbCalled = true;
  });

  assert.equal(ok, true);
  assert.equal(cbCalled, false);

  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(cbCalled, true);
  assert.equal(cbErr, undefined);
});

test("ws_safe: wsSendSafe respects ws.OPEN when present", () => {
  let sendCalled = false;
  const ws = {
    OPEN: 42,
    readyState: 42,
    send(data) {
      assert.equal(data, "hello");
      sendCalled = true;
    },
  };

  const ok = wsSendSafe(ws, "hello");
  assert.equal(ok, true);
  assert.equal(sendCalled, true);
});

test("ws_safe: wsSendSafe passes callback to ws-style rest-arg send()", () => {
  let cbCalled = false;
  const ws = {
    readyState: 1,
    terminate: () => {},
    send(data, ...args) {
      assert.equal(data, "hello");
      const cb = args.at(-1);
      assert.equal(typeof cb, "function");
      cbCalled = true;
    },
  };

  const ok = wsSendSafe(ws, "hello", () => {});
  assert.equal(ok, true);
  assert.equal(cbCalled, true);
});

test("ws_safe: wsSendSafe treats cb(null) as success for callback send()", async () => {
  let cbCalled = false;
  let cbErr;
  const ws = {
    readyState: 1,
    terminate: () => {},
    send(_data, cb) {
      cb(null);
    },
  };

  const ok = wsSendSafe(ws, "hello", (err) => {
    cbCalled = true;
    cbErr = err;
  });
  assert.equal(ok, true);

  if (!cbCalled) await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(cbCalled, true);
  assert.equal(cbErr, undefined);
});

test("ws_safe: wsSendSafe returns false for invalid ws input (and calls cb)", async () => {
  let cbCalled = false;
  let cbErr;

  const ok = wsSendSafe(null, "hello", (err) => {
    cbCalled = true;
    cbErr = err;
  });

  assert.equal(ok, false);
  assert.equal(cbCalled, false);

  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(cbCalled, true);
  assert.ok(cbErr instanceof Error);
});

test("ws_safe: wsSendSafe does not throw if send getter throws", async () => {
  const ws = { OPEN: 1, readyState: 1 };
  Object.defineProperty(ws, "send", {
    get() {
      throw new Error("boom");
    },
  });

  let cbCalled = false;
  let cbErr;
  const ok = wsSendSafe(ws, "hello", (err) => {
    cbCalled = true;
    cbErr = err;
  });
  assert.equal(ok, false);
  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(cbCalled, true);
  assert.ok(cbErr instanceof Error);
});

test("ws_safe: wsSendSafe does not throw if readyState getter throws", async () => {
  let sendCalled = false;
  const ws = { OPEN: 1, terminate: () => {} };
  Object.defineProperty(ws, "readyState", {
    get() {
      throw new Error("boom");
    },
  });
  ws.send = () => {
    sendCalled = true;
  };

  let cbCalled = false;
  let cbErr;
  const ok = wsSendSafe(ws, "hello", (err) => {
    cbCalled = true;
    cbErr = err;
  });
  assert.equal(ok, false);
  assert.equal(sendCalled, false);
  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(cbCalled, true);
  assert.ok(cbErr instanceof Error);
});

test("ws_safe: wsSendSafe does not throw if OPEN getter throws", () => {
  let sendCalled = false;
  const ws = { readyState: 1 };
  Object.defineProperty(ws, "OPEN", {
    get() {
      throw new Error("boom");
    },
  });
  ws.send = () => {
    sendCalled = true;
  };

  const ok = wsSendSafe(ws, "hello");
  assert.equal(ok, true);
  assert.equal(sendCalled, true);
});

test("ws_safe: wsCloseSafe is a no-op for invalid ws input", () => {
  wsCloseSafe(null);
  wsCloseSafe({});
});

test("ws_safe: wsCloseSafe treats empty reason as absent", () => {
  const calls = [];
  const ws = {
    close: (...args) => {
      calls.push(args);
    },
  };

  wsCloseSafe(ws, 1000, "");
  assert.deepEqual(calls, [[1000]]);
});

test("ws_safe: wsCloseSafe formats and UTF-8 byte-limits the close reason", () => {
  const calls = [];
  const ws = {
    close: (...args) => {
      calls.push(args);
    },
  };

  wsCloseSafe(ws, 1000, "hello\n\r\tworld " + "x".repeat(500));
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], 1000);
  assert.equal(typeof calls[0][1], "string");
  assert.ok(!/[\r\n]/u.test(calls[0][1]));
  assert.ok(Buffer.byteLength(calls[0][1], "utf8") <= 123);
});

test("ws_safe: wsCloseSafe does not throw on hostile reason inputs", () => {
  const calls = [];
  const ws = {
    close: (...args) => {
      calls.push(args);
    },
  };

  const hostile = { toString: () => { throw new Error("nope"); } };
  wsCloseSafe(ws, 1000, hostile);
  assert.deepEqual(calls, [[1000]]);
});

test("ws_safe: wsCloseSafe does not throw if close getter throws", () => {
  const ws = {};
  Object.defineProperty(ws, "close", {
    get() {
      throw new Error("boom");
    },
  });
  assert.doesNotThrow(() => wsCloseSafe(ws, 1000, "bye"));
});

test("ws_safe: wsIsOpenSafe does not throw if readyState getter throws", () => {
  const ws = { OPEN: 1 };
  Object.defineProperty(ws, "readyState", {
    get() {
      throw new Error("boom");
    },
  });
  assert.doesNotThrow(() => wsIsOpenSafe(ws));
  assert.equal(wsIsOpenSafe(ws), false);
});

test("ws_safe: wsIsOpenSafe returns false on invalid ws input", () => {
  assert.equal(wsIsOpenSafe(null), false);
  assert.equal(wsIsOpenSafe(undefined), false);
  assert.equal(wsIsOpenSafe(123), false);
});

test("ws_safe: wsIsOpenSafe returns true when readyState is not observable", () => {
  assert.equal(wsIsOpenSafe({}), true);
});
