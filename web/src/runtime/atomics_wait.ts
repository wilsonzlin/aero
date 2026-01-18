import { unrefBestEffort } from "../unrefSafe";

export type WaitUntilNotEqualResult = 'ok' | 'timed-out' | 'not-equal';

export interface WaitUntilNotEqualOptions {
  timeoutMs?: number;
  /**
   * Overrides whether the current context is allowed to use blocking
   * `Atomics.wait()`.
   *
   * Browsers disallow blocking waits on the Window (main/UI) thread.
   * This override exists primarily to let unit tests simulate "main-thread"
   * vs "worker" behavior regardless of the actual runtime.
   */
  canBlock?: boolean;
  /**
   * For tests: force-disable `Atomics.waitAsync()` usage even if the host
   * supports it, so the polling fallback can be exercised.
   */
  _useWaitAsync?: boolean;
  /** For tests: override sleep implementation used by polling fallback. */
  _sleep?: (ms: number) => Promise<void>;
  /** For tests: override clock used for timeout calculations. */
  _now?: () => number;
}

const DEFAULT_POLL_INTERVAL_MS = 16;

type AtomicsWaitAsync = (
  typedArray: Int32Array,
  index: number,
  value: number,
  timeout?: number,
) => { async: boolean; value: WaitUntilNotEqualResult | Promise<WaitUntilNotEqualResult> };

function isWindowContext(): boolean {
  // `document` is a strong signal that we're in the UI thread.
  return typeof document !== 'undefined';
}

function isDevBuild(): boolean {
  // Vite/Rollup style.
  if (typeof import.meta !== 'undefined') {
    const metaEnv = (import.meta as unknown as { env?: unknown }).env;
    if (metaEnv && typeof metaEnv === 'object') {
      const env = metaEnv as Record<string, unknown>;
      if (typeof env.DEV === 'boolean') return env.DEV;
    }
  }

  // Node style.
  if (typeof process !== 'undefined' && typeof process.env !== 'undefined') {
    return process.env.NODE_ENV !== 'production';
  }

  // Default to "dev" when we can't tell.
  return true;
}

function nowMs(): number {
  return typeof performance !== 'undefined' && typeof performance.now === 'function'
    ? performance.now()
    : Date.now();
}

function sleepMs(ms: number): Promise<void> {
  // On the Window thread, RAF-based polling avoids building up deep timer queues
  // and naturally yields to rendering/input. We only use it for "short" waits.
  if (typeof requestAnimationFrame === 'function' && ms <= DEFAULT_POLL_INTERVAL_MS) {
    return new Promise(resolve => requestAnimationFrame(() => resolve()));
  }

  return new Promise(resolve => {
    const timer = setTimeout(resolve, Math.max(0, ms));
    unrefBestEffort(timer);
  });
}

function remainingTimeoutMs(startMs: number, timeoutMs: number | undefined, now: () => number): number | undefined {
  if (timeoutMs === undefined) return undefined;

  const remaining = timeoutMs - (now() - startMs);
  return remaining <= 0 ? 0 : remaining;
}

export function notify(i32: Int32Array, index: number, count: number): number {
  return Atomics.notify(i32, index, count);
}

export async function waitUntilNotEqual(
  i32: Int32Array,
  index: number,
  value: number,
  options: WaitUntilNotEqualOptions = {},
): Promise<WaitUntilNotEqualResult> {
  const start = (options._now ?? nowMs)();
  const now = options._now ?? nowMs;

  if (Atomics.load(i32, index) !== value) return 'not-equal';

  const canBlock = options.canBlock ?? !isWindowContext();
  if (canBlock && isWindowContext() && isDevBuild()) {
    throw new Error(
      'Blocking Atomics.wait() is not allowed on the browser main thread. ' +
        'Use waitUntilNotEqual() without canBlock (or with canBlock: false) so it can ' +
        'fall back to Atomics.waitAsync() / polling.',
    );
  }

  if (canBlock) {
    // Worker-friendly path: use blocking Atomics.wait().
    while (Atomics.load(i32, index) === value) {
      const remaining = remainingTimeoutMs(start, options.timeoutMs, now);
      if (remaining === 0) return 'timed-out';

      const res =
        remaining === undefined ? Atomics.wait(i32, index, value) : Atomics.wait(i32, index, value, remaining);

      if (res === 'timed-out') return 'timed-out';
    }
    return 'ok';
  }

  const waitAsyncValue =
    options._useWaitAsync === false ? undefined : (Atomics as unknown as { waitAsync?: unknown }).waitAsync;
  const waitAsync = typeof waitAsyncValue === 'function' ? (waitAsyncValue as AtomicsWaitAsync) : undefined;

  if (waitAsync) {
    // Main thread safe path: Atomics.waitAsync() (woken by Atomics.notify()).
    while (Atomics.load(i32, index) === value) {
      const remaining = remainingTimeoutMs(start, options.timeoutMs, now);
      if (remaining === 0) return 'timed-out';

      const res = waitAsync(i32, index, value, remaining);
      const outcome = await Promise.resolve(res.value);

      if (outcome === 'timed-out') return 'timed-out';
    }
    return 'ok';
  }

  // Fallback: polling loop. This does *not* rely on Atomics.notify to wake up.
  const sleep = options._sleep ?? sleepMs;

  while (Atomics.load(i32, index) === value) {
    const remaining = remainingTimeoutMs(start, options.timeoutMs, now);
    if (remaining === 0) return 'timed-out';

    const delay = Math.min(DEFAULT_POLL_INTERVAL_MS, remaining ?? DEFAULT_POLL_INTERVAL_MS);
    await sleep(delay);
  }

  return 'ok';
}
