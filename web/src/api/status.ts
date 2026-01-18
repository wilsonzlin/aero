import type { AeroGlobalApi } from '../../../shared/aero_api.ts';
import { AERO_PHASES, isAeroPhase, type AeroPhase, type AeroStatusSnapshot } from '../../../shared/aero_status.ts';
import { unrefBestEffort } from '../unrefSafe';

export interface AeroStatusApi {
  status: AeroStatusSnapshot;
  events: EventTarget;
  setPhase(phase: AeroPhase): void;
  waitForPhase(phase: AeroPhase, options?: { timeoutMs?: number }): Promise<void>;
  emitEvent(name: string, detail?: unknown): void;
  waitForEvent<T = unknown>(name: string, options?: { timeoutMs?: number }): Promise<T>;
}

function isStatusSnapshot(value: unknown): value is AeroStatusSnapshot {
  if (!value || typeof value !== 'object') return false;
  const maybe = value as { phase?: unknown; phaseSinceMs?: unknown };
  return isAeroPhase(maybe.phase) && typeof maybe.phaseSinceMs === 'number';
}

function isEventTarget(value: unknown): value is EventTarget {
  if (!value || typeof value !== 'object') return false;
  const maybe = value as { addEventListener?: unknown; removeEventListener?: unknown; dispatchEvent?: unknown };
  return (
    typeof maybe.addEventListener === 'function' &&
    typeof maybe.removeEventListener === 'function' &&
    typeof maybe.dispatchEvent === 'function'
  );
}

function phaseAtLeast(current: AeroPhase, target: AeroPhase): boolean {
  return AERO_PHASES.indexOf(current) >= AERO_PHASES.indexOf(target);
}

export function initAeroStatusApi(initialPhase: AeroPhase = 'booting'): AeroStatusApi {
  const existing = window.aero;
  const aero: AeroGlobalApi =
    existing && typeof existing === 'object'
      ? (existing as AeroGlobalApi)
      : // If some consumer set `window.aero` to a non-object value, replace it so the API is usable.
        ((window.aero = {}) as AeroGlobalApi);

  const status: AeroStatusSnapshot = isStatusSnapshot(aero.status)
    ? aero.status
    : {
        phase: initialPhase,
        phaseSinceMs: performance.now(),
      };

  aero.status = status;

  const events = isEventTarget(aero.events) ? aero.events : new EventTarget();
  aero.events = events;

  function emitEvent(name: string, detail?: unknown) {
    events.dispatchEvent(new CustomEvent(name, { detail }));
  }

  function setPhase(phase: AeroPhase) {
    const prevPhase = status.phase;
    if (prevPhase === phase) return;

    status.phase = phase;
    status.phaseSinceMs = performance.now();

    emitEvent('phase_changed', { phase, prevPhase });
    emitEvent(`phase:${phase}`, { phase, prevPhase });

    // Convenience milestone events for automation harnesses.
    if (phase === 'desktop') emitEvent('desktop_ready', { phase });
    if (phase === 'idle') emitEvent('idle_ready', { phase });
  }

  function waitForEvent<T = unknown>(name: string, options?: { timeoutMs?: number }): Promise<T> {
    // Make common milestone waits race-free by resolving immediately when the
    // current status already satisfies the requested signal.
    if (name === 'desktop_ready') {
      if (phaseAtLeast(status.phase, 'desktop')) return Promise.resolve(undefined as T);
    } else if (name === 'idle_ready') {
      if (phaseAtLeast(status.phase, 'idle')) return Promise.resolve(undefined as T);
    } else if (name.startsWith('phase:')) {
      const rawPhase = name.slice('phase:'.length);
      if (isAeroPhase(rawPhase) && phaseAtLeast(status.phase, rawPhase)) {
        return Promise.resolve(undefined as T);
      }
    }

    return new Promise((resolve, reject) => {
      const listener = (event: Event) => {
        if (timeoutId !== undefined) clearTimeout(timeoutId);
        resolve((event as CustomEvent).detail as T);
      };

      events.addEventListener(name, listener, { once: true });

      const timeoutMs = options?.timeoutMs;
      const timeoutId =
        timeoutMs === undefined
          ? undefined
          : window.setTimeout(() => {
              events.removeEventListener(name, listener);
              reject(new Error(`Timed out waiting for aero event ${JSON.stringify(name)}`));
            }, timeoutMs);
      if (timeoutId !== undefined) {
        unrefBestEffort(timeoutId);
      }
    });
  }

  async function waitForPhase(phase: AeroPhase, options?: { timeoutMs?: number }): Promise<void> {
    if (status.phase === phase) return;
    await waitForEvent(`phase:${phase}`, options);
  }

  aero.setPhase = setPhase;
  aero.waitForPhase = waitForPhase;
  aero.emitEvent = emitEvent;
  aero.waitForEvent = waitForEvent;

  return { status, events, setPhase, waitForPhase, emitEvent, waitForEvent };
}
