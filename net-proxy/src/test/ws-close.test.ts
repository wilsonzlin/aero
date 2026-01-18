import test from "node:test";
import assert from "node:assert/strict";

import { wsCloseSafe, wsSendSafe } from "../wsClose";

function hasForbiddenControlChars(s: string): boolean {
  for (const ch of s) {
    const code = ch.codePointAt(0) ?? 0;
    if (code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029) {
      return true;
    }
  }
  return false;
}

test("wsCloseSafe sanitizes close reasons to avoid control characters", () => {
  let gotCode: number | null = null;
  let gotReason: string | null = null;

  const ws = {
    close(code: number, reason?: string) {
      gotCode = code;
      gotReason = reason ?? "";
    }
  } as unknown as import("ws").WebSocket;

  wsCloseSafe(ws, 1008, "bad\r\nreason\twith\u2028separators\u0000and\x7fcontrols");

  assert.equal(gotCode, 1008);
  const reason = gotReason ?? "";
  assert.equal(hasForbiddenControlChars(reason), false);
  assert.equal(reason.includes("\n"), false);
  assert.equal(reason.includes("\r"), false);
  assert.match(reason, /^bad reason with separators and controls$/);
});

test("wsCloseSafe terminates when close throws (close race hardening)", () => {
  let terminated = false;
  const ws = {
    close() {
      throw new Error("boom");
    },
    terminate() {
      terminated = true;
    }
  } as unknown as import("ws").WebSocket;

  wsCloseSafe(ws, 1000, "bye");
  assert.equal(terminated, true);
});

test("wsSendSafe treats cb(null) as success (ws callback convention)", async () => {
  let cbCalled = false;
  let cbErr: Error | undefined;
  let sendCalled = false;

  const ws = {
    OPEN: 1,
    readyState: 1,
    send(_data: unknown, cb?: (err: unknown) => void) {
      sendCalled = true;
      cb?.(null);
    }
  } as unknown as import("ws").WebSocket;

  const ok = wsSendSafe(ws, Buffer.from("hi"), (err) => {
    cbCalled = true;
    cbErr = err;
  });
  assert.equal(ok, true);
  assert.equal(sendCalled, true);

  if (!cbCalled) {
    await new Promise<void>((resolve) => queueMicrotask(() => resolve()));
  }
  assert.equal(cbCalled, true);
  assert.equal(cbErr, undefined);
});

test("wsSendSafe does not throw if send getter throws", async () => {
  let cbCalled = false;
  let cbErr: Error | undefined;

  const ws = {
    get send() {
      throw new Error("nope");
    }
  } as unknown as import("ws").WebSocket;

  assert.doesNotThrow(() => {
    const ok = wsSendSafe(ws, Buffer.from("hi"), (err) => {
      cbCalled = true;
      cbErr = err;
    });
    assert.equal(ok, false);
  });

  if (!cbCalled) {
    await new Promise<void>((resolve) => queueMicrotask(() => resolve()));
  }
  assert.equal(cbCalled, true);
  assert.equal(cbErr?.message, "Invalid WebSocket");
});

test("wsSendSafe does not throw if readyState getter throws", async () => {
  let cbCalled = false;
  let cbErr: Error | undefined;

  const ws = {
    OPEN: 1,
    get readyState() {
      throw new Error("boom");
    },
    send() {
      // should not be called
    }
  } as unknown as import("ws").WebSocket;

  assert.doesNotThrow(() => {
    const ok = wsSendSafe(ws, Buffer.from("hi"), (err) => {
      cbCalled = true;
      cbErr = err;
    });
    assert.equal(ok, false);
  });

  if (!cbCalled) {
    await new Promise<void>((resolve) => queueMicrotask(() => resolve()));
  }
  assert.equal(cbCalled, true);
  assert.equal(cbErr?.message, "WebSocket not open");
});

test("wsSendSafe does not throw if OPEN getter throws", async () => {
  let cbCalled = false;
  let cbErr: Error | undefined;
  let sendCalled = false;

  const ws = {
    get OPEN() {
      throw new Error("boom");
    },
    readyState: 1,
    send(_data: unknown, cb?: (err: unknown) => void) {
      sendCalled = true;
      cb?.(null);
    }
  } as unknown as import("ws").WebSocket;

  const ok = wsSendSafe(ws, Buffer.from("hi"), (err) => {
    cbCalled = true;
    cbErr = err;
  });
  assert.equal(ok, true);
  assert.equal(sendCalled, true);

  if (!cbCalled) {
    await new Promise<void>((resolve) => queueMicrotask(() => resolve()));
  }
  assert.equal(cbCalled, true);
  assert.equal(cbErr, undefined);
});

test("wsCloseSafe does not throw if close getter throws", () => {
  const ws = {
    get close() {
      throw new Error("boom");
    }
  } as unknown as import("ws").WebSocket;

  assert.doesNotThrow(() => {
    wsCloseSafe(ws, 1000, "bye");
  });
});

