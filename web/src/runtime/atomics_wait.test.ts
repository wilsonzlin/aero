import { Worker } from 'node:worker_threads';
import { describe, expect, it, vi } from 'vitest';

import { notify, waitUntilNotEqual } from './atomics_wait';

describe('waitUntilNotEqual', () => {
  it('returns "not-equal" immediately when value already differs', async () => {
    const sab = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
    const i32 = new Int32Array(sab);
    Atomics.store(i32, 0, 123);

    await expect(waitUntilNotEqual(i32, 0, 0, { canBlock: false })).resolves.toBe('not-equal');
  });

  it('main-thread mode does not call blocking Atomics.wait()', async () => {
    const sab = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
    const i32 = new Int32Array(sab);
    Atomics.store(i32, 0, 0);

    const waitSpy = vi.spyOn(Atomics, 'wait');

    const promise = waitUntilNotEqual(i32, 0, 0, { canBlock: false, timeoutMs: 500 });

    // Allow the waiter to arm itself.
    await Promise.resolve();

    Atomics.store(i32, 0, 1);
    notify(i32, 0, 1);

    await expect(promise).resolves.toBe('ok');
    expect(waitSpy).not.toHaveBeenCalled();
  });

  it('falls back to polling when Atomics.waitAsync is unavailable', async () => {
    const original = (Atomics as any).waitAsync as unknown;
    (Atomics as any).waitAsync = undefined;

    vi.useFakeTimers();
    try {
      const sab = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
      const i32 = new Int32Array(sab);
      Atomics.store(i32, 0, 0);

      const promise = waitUntilNotEqual(i32, 0, 0, { canBlock: false, timeoutMs: 500, _now: () => Date.now() });

      await vi.advanceTimersByTimeAsync(16);
      Atomics.store(i32, 0, 2);
      notify(i32, 0, 1);
      await vi.advanceTimersByTimeAsync(16);

      await expect(promise).resolves.toBe('ok');
    } finally {
      vi.useRealTimers();
      (Atomics as any).waitAsync = original as any;
    }
  });

  it('times out', async () => {
    const original = (Atomics as any).waitAsync as unknown;
    (Atomics as any).waitAsync = undefined;

    vi.useFakeTimers();
    try {
      const sab = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
      const i32 = new Int32Array(sab);
      Atomics.store(i32, 0, 0);

      const promise = waitUntilNotEqual(i32, 0, 0, { canBlock: false, timeoutMs: 50, _now: () => Date.now() });

      await vi.advanceTimersByTimeAsync(200);
      await expect(promise).resolves.toBe('timed-out');
    } finally {
      vi.useRealTimers();
      (Atomics as any).waitAsync = original as any;
    }
  });

  it(
    'blocking mode wakes when another thread stores+notifies',
    async () => {
      const sab = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
      const i32 = new Int32Array(sab);
      Atomics.store(i32, 0, 0);

      // Use an ESM worker so this test is stable across Node versions (some
      // versions treat eval workers as ESM by default under `"type": "module"`).
      const notifier = new Worker(
        `
        import { parentPort, workerData } from 'node:worker_threads';
        const i32 = new Int32Array(workerData.sab);
        parentPort?.postMessage('ready');
        setTimeout(() => {
          Atomics.store(i32, 0, 1);
          Atomics.notify(i32, 0, 1);
        }, 10);
      `,
        { eval: true, type: 'module', workerData: { sab } },
      );

      try {
        // Ensure the worker thread is fully initialized before we enter the
        // blocking Atomics.wait() path. Under heavy load, starting the worker
        // can take long enough that a short wait timeout flakes.
        await new Promise<void>((resolve) => {
          notifier.once('message', () => resolve());
        });
        await expect(waitUntilNotEqual(i32, 0, 0, { canBlock: true, timeoutMs: 5_000 })).resolves.toBe('ok');
      } finally {
        await notifier.terminate();
      }
    },
    20_000,
  );
});
