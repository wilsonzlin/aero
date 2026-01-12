import { InputEventRouter } from '../input/event_router';
import { InMemoryInputQueue } from '../input/queue';
import { drainInputQueue } from '../input/worker_consumer';
import { installFallbackPerf } from './fallback';
import type { PerfApi } from './types';

export interface SyntheticInputLatencyTestOptions {
  event_count?: number;
  consume_delay_ms?: number;
}

export async function runSyntheticInputLatencyTest(
  opts: SyntheticInputLatencyTestOptions = {},
): Promise<{ perf: PerfApi; export: unknown }> {
  if (typeof window === 'undefined') throw new Error('Synthetic test requires a browser window context.');

  // Be defensive: other tooling might set `window.aero` to a non-object value.
  // Align with `web/src/api/status.ts` / net-trace backend installers.
  const win = window as unknown as { aero?: unknown };
  if (!win.aero || typeof win.aero !== 'object') {
    win.aero = {};
  }
  const aero = win.aero as NonNullable<Window['aero']>;
  const perf: PerfApi =
    aero.perf && typeof aero.perf === 'object' && 'getHudSnapshot' in aero.perf ? (aero.perf as PerfApi) : installFallbackPerf();
  aero.perf = perf;

  const eventCount = opts.event_count ?? 50;
  const consumeDelayMs = opts.consume_delay_ms ?? 4;

  perf.setHudActive(true);

  const target = document.createElement('div');
  target.tabIndex = 0;
  target.style.position = 'fixed';
  target.style.left = '0';
  target.style.top = '0';
  target.style.width = '1px';
  target.style.height = '1px';
  target.style.opacity = '0';
  document.body.appendChild(target);

  const queue = new InMemoryInputQueue();
  const router = new InputEventRouter({
    target,
    queue,
    hooks: {
      on_capture: ({ id, t_capture_ms }) => perf.noteInputCaptured?.(id, t_capture_ms),
      on_injected: ({ id, t_injected_ms, queue: qs, enqueued }) => {
        if (!enqueued) return;
        perf.noteInputInjected?.(id, t_injected_ms, qs.depth, qs.oldest_capture_ms);
      },
    },
  });
  router.start();

  const drain = () => {
    drainInputQueue(
      queue,
      () => {},
      {
        on_consumed: ({ id, t_consumed_ms, queue: qs }) => {
          perf.noteInputConsumed?.(id, t_consumed_ms, qs.depth, qs.oldest_capture_ms);
        },
      },
    );
  };

  for (let i = 0; i < eventCount; i += 1) {
    target.dispatchEvent(new KeyboardEvent('keydown', { code: 'KeyA', key: 'a' }));
    setTimeout(drain, consumeDelayMs);
    await new Promise((r) => setTimeout(r, 0));
  }

  await new Promise<void>((r) => requestAnimationFrame(() => r()));
  await new Promise<void>((r) => requestAnimationFrame(() => r()));

  router.stop();
  target.remove();

  perf.setHudActive(false);

  return { perf, export: perf.export() };
}
