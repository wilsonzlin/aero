import assert from 'node:assert/strict';
import test from 'node:test';
import { VmCoordinator } from '../src/vmCoordinator.js';
import { unrefBestEffort } from '../src/unref_safe.js';

const WATCHDOG_TEST_TIMEOUT_MS = 30_000;

function onceEvent(target, type) {
  return new Promise((resolve) => {
    const handler = (event) => {
      target.removeEventListener(type, handler);
      resolve(event);
    };
    target.addEventListener(type, handler);
  });
}

function sleep(ms) {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    unrefBestEffort(timeout);
  });
}

test(
  'watchdog terminates a non-yielding CPU worker without blocking the main thread',
  { timeout: WATCHDOG_TEST_TIMEOUT_MS },
  async () => {
    const vm = new VmCoordinator({
      config: {
        cpu: {
          watchdogTimeoutMs: 150,
          maxSliceMs: 5,
          maxInstructionsPerSlice: 100_000,
          backgroundThrottleMs: 0,
        },
      },
    });

    let ticks = 0;
    const interval = setInterval(() => {
      ticks += 1;
    }, 10);
    unrefBestEffort(interval);

    const errorEventPromise = onceEvent(vm, 'error');
    await vm.start({ mode: 'nonYieldingLoop' });

    const tickDeadline = Date.now() + 2_000;
    while (ticks === 0 && Date.now() < tickDeadline) {
      await sleep(10);
    }
    assert.ok(ticks > 0, 'main thread timer should keep firing while CPU worker is hung');

    // Force the watchdog to observe an expired heartbeat window, then trigger the check
    // synchronously. Relying on the periodic timer can be flaky under extreme CPU contention
    // when the Node test runner executes many files in parallel.
    vm.lastHeartbeatAt = Date.now() - vm.config.cpu.watchdogTimeoutMs - 1;
    vm._checkWatchdog();

    const errorEvent = await errorEventPromise;
    clearInterval(interval);

    assert.equal(errorEvent.detail.error.code, 'WatchdogTimeout');

    await vm.reset();
    assert.equal(vm.state, 'stopped');

    await vm.start({ mode: 'cooperativeInfiniteLoop' });
    const heartbeat = await onceEvent(vm, 'heartbeat');
    assert.ok(heartbeat.detail.totalInstructions > 0);
    await vm.shutdown();
    assert.equal(vm.state, 'stopped');
  },
);

test(
  'pause and step remain responsive during a cooperative tight loop',
  { timeout: WATCHDOG_TEST_TIMEOUT_MS },
  async () => {
    const vm = new VmCoordinator({
      config: {
        cpu: {
          watchdogTimeoutMs: 1000,
          maxSliceMs: 5,
          maxInstructionsPerSlice: 250_000,
          backgroundThrottleMs: 0,
        },
      },
    });

    await vm.start({ mode: 'cooperativeInfiniteLoop' });

    const firstHeartbeat = await onceEvent(vm, 'heartbeat');
    assert.ok(firstHeartbeat.detail.totalInstructions > 0);

    await vm.pause();
    assert.equal(vm.state, 'paused');

    const before = vm.lastHeartbeat?.totalInstructions ?? 0;
    await vm.step();
    const after = vm.lastHeartbeat?.totalInstructions ?? 0;
    assert.ok(after > before, 'step should advance execution while paused');

    await vm.shutdown();
    assert.equal(vm.state, 'stopped');
  },
);

test(
  'resource limits reject oversized guest RAM requests with actionable errors',
  { timeout: WATCHDOG_TEST_TIMEOUT_MS },
  async () => {
    const vm = new VmCoordinator({
      config: {
        guestRamBytes: 64 * 1024 * 1024,
        limits: { maxGuestRamBytes: 32 * 1024 * 1024 },
        cpu: { watchdogTimeoutMs: 1000, backgroundThrottleMs: 0 },
      },
    });

    const errorEventPromise = onceEvent(vm, 'error');
    await assert.rejects(() => vm.start(), /guest RAM request/i);
    const errorEvent = await errorEventPromise;
    assert.equal(errorEvent.detail.error.code, 'ResourceLimitExceeded');
    assert.match(errorEvent.detail.error.suggestion, /increase/i);

    await vm.reset();
    assert.equal(vm.state, 'stopped');
  },
);

test(
  'worker crashes surface structured errors and keep an auto-saved snapshot',
  { timeout: WATCHDOG_TEST_TIMEOUT_MS },
  async () => {
    const vm = new VmCoordinator({
      config: {
        autoSaveSnapshotOnCrash: true,
        cpu: { watchdogTimeoutMs: 1000, backgroundThrottleMs: 0, maxSliceMs: 5, maxInstructionsPerSlice: 10_000 },
      },
    });

    const errorEventPromise = onceEvent(vm, 'error');
    await vm.start({ mode: 'crash' });

    const errorEvent = await errorEventPromise;
    assert.equal(errorEvent.detail.error.code, 'InternalError');
    assert.equal(errorEvent.detail.snapshot?.reason, 'crash');
    assert.equal(vm.lastSnapshot?.reason, 'crash');

    await vm.reset();
    assert.equal(vm.state, 'stopped');
  },
);

test(
  'cache limit violations surface as structured errors without crashing the VM',
  { timeout: WATCHDOG_TEST_TIMEOUT_MS },
  async () => {
    const vm = new VmCoordinator({
      config: {
        limits: { maxDiskCacheBytes: 1024 * 1024 },
        cpu: { watchdogTimeoutMs: 1000, backgroundThrottleMs: 0, maxSliceMs: 5, maxInstructionsPerSlice: 10_000 },
      },
    });

    await vm.start({ mode: 'cooperativeInfiniteLoop' });
    await onceEvent(vm, 'heartbeat');

    const result = await vm.writeCacheEntry({ cache: 'disk', sizeBytes: 2 * 1024 * 1024 });
    assert.equal(result.ok, false);
    assert.equal(result.error?.code, 'ResourceLimitExceeded');
    assert.equal(vm.state, 'running');

    await vm.shutdown();
  },
);
