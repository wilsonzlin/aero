// SPDX-License-Identifier: MIT OR Apache-2.0

import assert from "node:assert/strict";
import test from "node:test";

import {
  parseIoAipcWorkerInitMessage,
  parseIoWorkerInitMessage,
  parseSerialDemoCpuWorkerInitMessage,
} from "../web/src/workers/worker_init_parsers.ts";

function makeHostileThrowingProxy() {
  return new Proxy(
    {},
    {
      get() {
        throw new Error("boom");
      },
      getOwnPropertyDescriptor() {
        throw new Error("boom");
      },
      has() {
        throw new Error("boom");
      },
      ownKeys() {
        throw new Error("boom");
      },
    },
  );
}

test("worker init parsers: never throw on hostile proxy data", () => {
  const hostile = makeHostileThrowingProxy();
  assert.doesNotThrow(() => parseIoWorkerInitMessage(hostile));
  assert.doesNotThrow(() => parseIoAipcWorkerInitMessage(hostile));
  assert.doesNotThrow(() => parseSerialDemoCpuWorkerInitMessage(hostile));
});

test("worker init parsers: reject non-object inputs", () => {
  assert.equal(parseIoWorkerInitMessage(null), null);
  assert.equal(parseIoWorkerInitMessage(123), null);
  assert.equal(parseIoWorkerInitMessage("init"), null);
});

test("worker init parsers: accept valid io_worker init", () => {
  const sab = new SharedArrayBuffer(16);
  const out = parseIoWorkerInitMessage({ type: "init", requestRing: sab, responseRing: sab, tickIntervalMs: 1 });
  assert.ok(out);
  assert.equal(out.requestRing, sab);
  assert.equal(out.responseRing, sab);
  assert.equal(out.tickIntervalMs, 1);
});

test("worker init parsers: accept valid io_aipc init and defaults optional fields", () => {
  const sab = new SharedArrayBuffer(16);
  const out = parseIoAipcWorkerInitMessage({ type: "init", ipcBuffer: sab });
  assert.ok(out);
  assert.equal(out.ipcBuffer, sab);
  assert.equal(typeof out.cmdKind, "number");
  assert.equal(typeof out.evtKind, "number");
  assert.equal(out.tickIntervalMs, 5);
  assert.deepEqual(out.devices, ["i8042"]);
});

test("worker init parsers: accept valid serial demo init and defaults text", () => {
  const sab = new SharedArrayBuffer(16);
  const out = parseSerialDemoCpuWorkerInitMessage({ type: "init", requestRing: sab, responseRing: sab });
  assert.ok(out);
  assert.equal(out.requestRing, sab);
  assert.equal(out.responseRing, sab);
  assert.equal(typeof out.text, "string");
  assert.ok(out.text.length > 0);
});

