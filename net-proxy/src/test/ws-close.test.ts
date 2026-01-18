import test from "node:test";
import assert from "node:assert/strict";

import { wsCloseSafe, wsIsOpenSafe, wsSendSafe } from "../wsClose";

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

test("wsCloseSafe treats empty reason as absent", () => {
  const calls: unknown[][] = [];
  const ws = {
    close: (...args: unknown[]) => {
      calls.push(args);
    },
  } as unknown as import("ws").WebSocket;

  wsCloseSafe(ws, 1000, "");
  assert.deepEqual(calls, [[1000]]);
});

test("wsCloseSafe does not throw on hostile reason inputs", () => {
  const calls: unknown[][] = [];
  const ws = {
    close: (...args: unknown[]) => {
      calls.push(args);
    },
  } as unknown as import("ws").WebSocket;

  const hostile = {
    toString: () => {
      throw new Error("nope");
    },
  };
  assert.doesNotThrow(() => wsCloseSafe(ws, 1000, hostile));
  assert.deepEqual(calls, [[1000]]);
});

test("wsCloseSafe is a no-op for invalid ws input", () => {
  assert.doesNotThrow(() => wsCloseSafe(null as unknown as import("ws").WebSocket, 1000, "bye"));
  assert.doesNotThrow(() => wsCloseSafe({} as unknown as import("ws").WebSocket, 1000, "bye"));
});

test("wsIsOpenSafe returns false on invalid ws input", () => {
  assert.equal(wsIsOpenSafe(null), false);
  assert.equal(wsIsOpenSafe(undefined), false);
});

test("wsIsOpenSafe returns true when readyState is not observable", () => {
  const ws = {} as unknown as import("ws").WebSocket;
  assert.equal(wsIsOpenSafe(ws), true);
});

test("wsIsOpenSafe respects ws.OPEN when present", () => {
  assert.equal(wsIsOpenSafe({ OPEN: 42, readyState: 42 } as unknown as import("ws").WebSocket), true);
  assert.equal(wsIsOpenSafe({ OPEN: 42, readyState: 1 } as unknown as import("ws").WebSocket), false);
});

test("wsIsOpenSafe does not throw if OPEN getter throws", () => {
  const ws = { readyState: 1 } as unknown as import("ws").WebSocket;
  Object.defineProperty(ws as object, "OPEN", {
    get() {
      throw new Error("boom");
    },
  });
  assert.equal(wsIsOpenSafe(ws), true);
});

test("wsIsOpenSafe returns false when readyState getter throws", () => {
  const ws = {} as unknown as import("ws").WebSocket;
  Object.defineProperty(ws as object, "readyState", {
    get() {
      throw new Error("boom");
    },
  });
  assert.equal(wsIsOpenSafe(ws), false);
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

test("wsSendSafe returns false for invalid ws input (and calls cb)", async () => {
  let cbCalled = false;
  let cbErr: Error | undefined;

  const ok = wsSendSafe(null as unknown as import("ws").WebSocket, Buffer.from("hi"), (err) => {
    cbCalled = true;
    cbErr = err;
  });

  assert.equal(ok, false);
  assert.equal(cbCalled, false);

  await new Promise<void>((resolve) => queueMicrotask(() => resolve()));
  assert.equal(cbCalled, true);
  assert.ok(cbErr instanceof Error);
  assert.equal(cbErr.message, "Invalid WebSocket");
});

test("wsSendSafe does not pass callback to 1-arg send()", async () => {
  let cbCalled = false;
  let cbErr: Error | undefined;
  let sendArgsCount = -1;

  const ws = {
    OPEN: 1,
    readyState: 1,
    send: function (_data: unknown) {
      sendArgsCount = arguments.length;
    }
  } as unknown as import("ws").WebSocket;

  const ok = wsSendSafe(ws, Buffer.from("hi"), (err) => {
    cbCalled = true;
    cbErr = err;
  });

  assert.equal(ok, true);
  assert.equal(sendArgsCount, 1);
  assert.equal(cbCalled, false);

  await new Promise<void>((resolve) => queueMicrotask(() => resolve()));
  assert.equal(cbCalled, true);
  assert.equal(cbErr, undefined);
});

test("wsSendSafe passes callback to ws-style rest-arg send()", () => {
  let cbCalled = false;
  let sendCalled = false;

  const ws = {
    readyState: 1,
    terminate() {},
    send(_data: unknown, ...args: unknown[]) {
      sendCalled = true;
      const cb = args.at(-1);
      assert.equal(typeof cb, "function");
      cbCalled = true;
    }
  } as unknown as import("ws").WebSocket;

  const ok = wsSendSafe(ws, Buffer.from("hi"), () => {});
  assert.equal(ok, true);
  assert.equal(sendCalled, true);
  assert.equal(cbCalled, true);
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

