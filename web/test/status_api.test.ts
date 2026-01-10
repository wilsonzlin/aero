import test from 'node:test';
import assert from 'node:assert/strict';

import { initAeroStatusApi } from '../src/api/status.ts';

function withNodeWindow<T>(fn: () => T): T {
  const prevWindow = (globalThis as unknown as { window?: unknown }).window;
  const prevAero = (globalThis as unknown as { aero?: unknown }).aero;

  // Make `window` available as a global binding (Node doesn't define it by default).
  (globalThis as unknown as { window: unknown }).window = globalThis;

  try {
    return fn();
  } finally {
    if (prevWindow === undefined) {
      delete (globalThis as unknown as { window?: unknown }).window;
    } else {
      (globalThis as unknown as { window: unknown }).window = prevWindow;
    }

    if (prevAero === undefined) {
      delete (globalThis as unknown as { aero?: unknown }).aero;
    } else {
      (globalThis as unknown as { aero: unknown }).aero = prevAero;
    }
  }
}

test('initAeroStatusApi merges into existing window.aero', () => {
  withNodeWindow(() => {
    const perf = { export: () => ({ ok: true }) };
    (window as unknown as { aero?: any }).aero = { perf };

    const api = initAeroStatusApi('booting');
    assert.equal(api.status.phase, 'booting');
    assert.equal((window as unknown as { aero?: any }).aero.perf, perf);
  });
});

test('setPhase is idempotent', () => {
  withNodeWindow(() => {
    const api = initAeroStatusApi('booting');
    const since = api.status.phaseSinceMs;
    api.setPhase('booting');
    assert.equal(api.status.phase, 'booting');
    assert.equal(api.status.phaseSinceMs, since);
  });
});

test('waitForPhase resolves when phase changes', async () => {
  await withNodeWindow(async () => {
    const api = initAeroStatusApi('booting');

    const pending = api.waitForPhase('desktop', { timeoutMs: 1000 });
    api.setPhase('desktop');
    await pending;
  });
});

test('waitForEvent resolves immediately when phase already satisfies milestone', async () => {
  await withNodeWindow(async () => {
    const api = initAeroStatusApi('idle');
    await api.waitForEvent('desktop_ready', { timeoutMs: 10 });
    await api.waitForEvent('idle_ready', { timeoutMs: 10 });
    await api.waitForEvent('phase:desktop', { timeoutMs: 10 });
    await api.waitForEvent('phase:booting', { timeoutMs: 10 });
  });
});

