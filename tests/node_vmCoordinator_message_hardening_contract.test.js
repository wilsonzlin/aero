import test from 'node:test';
import assert from 'node:assert/strict';

import { VmCoordinator } from '../src/vmCoordinator.js';
import { withCustomEventOverride } from './helpers/custom_event_env.js';

test('node VmCoordinator: _onWorkerMessage does not throw on hostile proxy messages', () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error('boom');
      },
      getPrototypeOf() {
        throw new Error('boom');
      },
    },
  );

  assert.doesNotThrow(() => vm._onWorkerMessage(hostile));
});

test('node VmCoordinator: heartbeat handling stores a safe snapshot (no raw proxy retention)', () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const msg = new Proxy(
    { type: 'heartbeat' },
    {
      get(_target, prop) {
        if (prop === 'type') return 'heartbeat';
        throw new Error('boom');
      },
    },
  );

  assert.doesNotThrow(() => vm._onWorkerMessage(msg));
  assert.ok(vm.lastHeartbeat);
  assert.equal(vm.lastHeartbeat.type, 'heartbeat');
  assert.equal(typeof vm.lastHeartbeat.executed, 'number');
  assert.equal(typeof vm.lastHeartbeat.totalInstructions, 'number');
  assert.notEqual(vm.lastHeartbeat, msg);
  assert.ok(vm.lastSnapshot);
  assert.equal(vm.lastSnapshot.reason, 'heartbeat');
});

test('node VmCoordinator: _emitError does not throw on hostile proxy errors', () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });
  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error('boom');
      },
      getPrototypeOf() {
        throw new Error('boom');
      },
    },
  );

  assert.doesNotThrow(() => vm._emitError(hostile));
});

test('node VmCoordinator: _emitError produces a structured error object with bounded strings', () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });

  let detail = null;
  vm.addEventListener('error', (ev) => {
    detail = ev.detail;
  });

  vm._emitError({ code: 'X', name: 'Y', message: 'hello\nworld' });

  assert.ok(detail);
  assert.equal(detail.error.code, 'X');
  assert.equal(detail.error.name, 'Y');
  assert.equal(detail.error.message.includes('\n'), false);
});

test('node VmCoordinator: error/heartbeat events still deliver ev.detail without CustomEvent', () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });

  let heartbeatDetail = null;
  let errorDetail = null;

  vm.addEventListener('heartbeat', (ev) => {
    heartbeatDetail = ev.detail;
  });
  vm.addEventListener('error', (ev) => {
    errorDetail = ev.detail;
  });

  withCustomEventOverride(undefined, () => {
    vm._onWorkerMessage({ type: 'heartbeat', at: 1, executed: 2, totalInstructions: 3, pc: 4, resources: {} });
    vm._emitError({ code: 'X', name: 'Y', message: 'm' });
  });

  assert.ok(heartbeatDetail);
  assert.equal(heartbeatDetail.type, 'heartbeat');
  assert.ok(errorDetail);
  assert.equal(errorDetail.error.code, 'X');
});

test('node VmCoordinator: events still deliver ev.detail when CustomEvent constructor throws', () => {
  const vm = new VmCoordinator({ config: { autoSaveSnapshotOnCrash: false } });

  let heartbeatDetail = null;
  vm.addEventListener('heartbeat', (ev) => {
    heartbeatDetail = ev.detail;
  });

  function ThrowingCustomEvent() {
    throw new Error('nope');
  }

  withCustomEventOverride(ThrowingCustomEvent, () => {
    vm._onWorkerMessage({ type: 'heartbeat', at: 1, executed: 2, totalInstructions: 3, pc: 4, resources: {} });
  });

  assert.ok(heartbeatDetail);
  assert.equal(heartbeatDetail.type, 'heartbeat');
});
