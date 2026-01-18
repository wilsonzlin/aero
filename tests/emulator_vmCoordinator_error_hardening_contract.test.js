import test from "node:test";
import assert from "node:assert/strict";

import { VmCoordinator } from "../src/emulator/vmCoordinator.js";
import { withCustomEventOverride } from "./helpers/custom_event_env.js";

test("emulator/vmCoordinator: _emitError does not throw on hostile proxy errors", () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const hostile = new Proxy(
    {},
    {
      getPrototypeOf() {
        throw new Error("boom");
      },
    },
  );

  assert.doesNotThrow(() => vm._emitError(hostile));
  assert.ok(vm.lastError);
  assert.equal(typeof vm.lastError.error.message, "string");
});

test("emulator/vmCoordinator: _onWorkerError does not throw when event.error instanceof Error would throw", () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const hostile = new Proxy(
    {},
    {
      getPrototypeOf() {
        throw new Error("boom");
      },
    },
  );

  const event = { error: hostile, message: "load failed", filename: "x.js", lineno: 1, colno: 2 };
  assert.doesNotThrow(() => vm._onWorkerError(event));
  assert.ok(vm.lastError);
  assert.equal(vm.lastError.error.code, "WorkerCrashed");
});

test("emulator/vmCoordinator: _onWorkerMessage does not throw on hostile proxy messages", () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error("boom");
      },
      getPrototypeOf() {
        throw new Error("boom");
      },
    },
  );

  assert.doesNotThrow(() => vm._onWorkerMessage(hostile));
});

test("emulator/vmCoordinator: heartbeat handling stores a safe snapshot (no raw proxy retention)", () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const msg = new Proxy(
    { type: "heartbeat" },
    {
      get(_target, prop) {
        if (prop === "type") return "heartbeat";
        throw new Error("boom");
      },
    },
  );

  assert.doesNotThrow(() => vm._onWorkerMessage(msg));
  assert.ok(vm.lastHeartbeat);
  assert.equal(vm.lastHeartbeat.type, "heartbeat");
  assert.equal(typeof vm.lastHeartbeat.executed, "number");
  assert.equal(typeof vm.lastHeartbeat.totalInstructions, "number");
  assert.equal(vm.lastHeartbeat.mic, null);
  assert.ok(vm.lastSnapshot);
  assert.equal(vm.lastSnapshot.reason, "heartbeat");
});

test("emulator/vmCoordinator: heartbeat sanitization preserves mic sampleRate when present", () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const msg = {
    type: "heartbeat",
    at: 1,
    executed: 2,
    totalInstructions: 3,
    pc: 4,
    resources: { guestRamBytes: 5, diskCacheBytes: 6, shaderCacheBytes: 7 },
    mic: { rms: 0.25, dropped: 9, sampleRate: 48_000 },
  };

  vm._onWorkerMessage(msg);
  assert.ok(vm.lastHeartbeat);
  assert.deepEqual(vm.lastHeartbeat.mic, { rms: 0.25, dropped: 9, sampleRate: 48_000 });
});

test("emulator/vmCoordinator: error/heartbeat events still deliver ev.detail without CustomEvent", () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });

  let heartbeatDetail = null;
  let errorDetail = null;

  vm.addEventListener("heartbeat", (ev) => {
    heartbeatDetail = ev.detail;
  });
  vm.addEventListener("error", (ev) => {
    errorDetail = ev.detail;
  });

  withCustomEventOverride(undefined, () => {
    vm._onWorkerMessage({ type: "heartbeat", at: 1, executed: 2, totalInstructions: 3, pc: 4, resources: {}, mic: null });
    vm._emitError({ code: "X", name: "Y", message: "m" });
  });

  assert.ok(heartbeatDetail);
  assert.equal(heartbeatDetail.type, "heartbeat");
  assert.ok(errorDetail);
  assert.equal(errorDetail.error.code, "X");
});

test("emulator/vmCoordinator: events still deliver ev.detail when CustomEvent constructor throws", () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });

  let heartbeatDetail = null;
  vm.addEventListener("heartbeat", (ev) => {
    heartbeatDetail = ev.detail;
  });

  function ThrowingCustomEvent() {
    throw new Error("nope");
  }

  withCustomEventOverride(ThrowingCustomEvent, () => {
    vm._onWorkerMessage({ type: "heartbeat", at: 1, executed: 2, totalInstructions: 3, pc: 4, resources: {}, mic: null });
  });

  assert.ok(heartbeatDetail);
  assert.equal(heartbeatDetail.type, "heartbeat");
});
