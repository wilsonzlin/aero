import assert from "node:assert/strict";
import test from "node:test";

import { endThenDestroyQuietly } from "../src/socket_end_then_destroy.js";
import socketEndThenDestroyCjs from "../src/socket_end_then_destroy.cjs";

const implementations = [
  { name: "esm", endThenDestroyQuietly },
  { name: "cjs", endThenDestroyQuietly: socketEndThenDestroyCjs.endThenDestroyQuietly },
];

for (const impl of implementations) {
  test(`socket_end_then_destroy (${impl.name}): calls end() and schedules unref'd destroy fallback`, () => {
    const calls = [];
    const once = [];

    let timeoutFn;
    let timeoutMs;
    let unrefCalled = false;
    let clearedWith;

    let endCalls = 0;

    const prevSetTimeout = globalThis.setTimeout;
    const prevClearTimeout = globalThis.clearTimeout;
    globalThis.setTimeout = (fn, ms) => {
      timeoutFn = fn;
      timeoutMs = ms;
      return {
        unref() {
          unrefCalled = true;
        },
      };
    };
    globalThis.clearTimeout = (t) => {
      clearedWith = t;
    };

    try {
      const socket = {
        end(data) {
          endCalls += 1;
          calls.push(["end", data]);
        },
        destroy() {
          calls.push(["destroy"]);
        },
        once(event, fn) {
          once.push([event, fn]);
        },
      };

      impl.endThenDestroyQuietly(socket, "hello", { timeoutMs: 123 });
      // Idempotent: repeated calls do not register extra timers/listeners.
      impl.endThenDestroyQuietly(socket, "hello2", { timeoutMs: 123 });

      assert.deepEqual(calls, [["end", "hello"]]);
      assert.equal(endCalls, 1);
      assert.equal(timeoutMs, 123);
      assert.equal(typeof timeoutFn, "function");
      assert.equal(unrefCalled, true);

      const closeHandler = once.find((x) => x[0] === "close")?.[1];
      const errorHandler = once.find((x) => x[0] === "error")?.[1];
      assert.equal(typeof closeHandler, "function");
      assert.equal(typeof errorHandler, "function");

      timeoutFn();
      assert.deepEqual(calls, [["end", "hello"], ["destroy"]]);

      closeHandler();
      errorHandler();
      assert.ok(clearedWith, "expected clearTimeout to be called");

      // Even after cleanup runs, repeated calls remain no-ops.
      impl.endThenDestroyQuietly(socket, "hello3", { timeoutMs: 123 });
      assert.deepEqual(calls, [["end", "hello"], ["destroy"]]);
    } finally {
      globalThis.setTimeout = prevSetTimeout;
      globalThis.clearTimeout = prevClearTimeout;
    }
  });

  test(`socket_end_then_destroy (${impl.name}): does not throw if end getter throws (hostile socket)`, () => {
    let timeoutCalls = 0;
    const prevSetTimeout = globalThis.setTimeout;
    globalThis.setTimeout = () => {
      timeoutCalls += 1;
      throw new Error("unexpected timer");
    };

    try {
      const socket = {
        get end() {
          throw new Error("boom");
        },
        once() {
          throw new Error("unexpected once");
        },
      };
      assert.doesNotThrow(() => impl.endThenDestroyQuietly(socket, "hello", { timeoutMs: 123 }));
      assert.equal(timeoutCalls, 0);
    } finally {
      globalThis.setTimeout = prevSetTimeout;
    }
  });

  test(`socket_end_then_destroy (${impl.name}): does not throw if timer.unref getter throws`, () => {
    const calls = [];
    let timeoutFn;
    let timeoutMs;

    const prevSetTimeout = globalThis.setTimeout;
    globalThis.setTimeout = (fn, ms) => {
      timeoutFn = fn;
      timeoutMs = ms;
      return {
        get unref() {
          throw new Error("boom");
        },
      };
    };

    try {
      const socket = {
        end(data) {
          calls.push(["end", data]);
        },
        destroy() {
          calls.push(["destroy"]);
        },
        once() {},
      };

      assert.doesNotThrow(() => impl.endThenDestroyQuietly(socket, "hello", { timeoutMs: 50 }));
      assert.deepEqual(calls, [["end", "hello"]]);
      assert.equal(timeoutMs, 50);
      assert.equal(typeof timeoutFn, "function");

      timeoutFn();
      assert.deepEqual(calls, [["end", "hello"], ["destroy"]]);
    } finally {
      globalThis.setTimeout = prevSetTimeout;
    }
  });

  test(`socket_end_then_destroy (${impl.name}): destroys immediately if end() throws`, () => {
    const calls = [];
    let timeoutCalls = 0;
    const prevSetTimeout = globalThis.setTimeout;
    globalThis.setTimeout = () => {
      timeoutCalls += 1;
      throw new Error("unexpected timer");
    };
    const socket = {
      end() {
        throw new Error("boom");
      },
      destroy() {
        calls.push("destroy");
      },
      once() {},
    };

    try {
      impl.endThenDestroyQuietly(socket, "hello");
      assert.deepEqual(calls, ["destroy"]);
      assert.equal(timeoutCalls, 0);
    } finally {
      globalThis.setTimeout = prevSetTimeout;
    }
  });

  test(`socket_end_then_destroy (${impl.name}): does not schedule a timer if close fires during end()`, () => {
    const calls = [];
    const onceByEvent = new Map();

    let timeoutCalls = 0;
    const prevSetTimeout = globalThis.setTimeout;
    globalThis.setTimeout = () => {
      timeoutCalls += 1;
      throw new Error("unexpected timer");
    };

    try {
      const socket = {
        end(data) {
          calls.push(["end", data]);
          onceByEvent.get("close")?.();
        },
        destroy() {
          calls.push(["destroy"]);
        },
        once(event, fn) {
          onceByEvent.set(event, fn);
        },
      };

      impl.endThenDestroyQuietly(socket, "hello", { timeoutMs: 123 });
      assert.deepEqual(calls, [["end", "hello"]]);
      assert.equal(timeoutCalls, 0);
    } finally {
      globalThis.setTimeout = prevSetTimeout;
    }
  });
}
