import type { AeroGlobalApi } from '../../../shared/aero_api.ts';
import { isAeroPhase, type AeroPhase, type AeroStatusSnapshot } from '../../../shared/aero_status.ts';

export interface AeroStatusApi {
  status: AeroStatusSnapshot;
  events: EventTarget;
  setPhase(phase: AeroPhase): void;
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
    status.phase = phase;
    status.phaseSinceMs = performance.now();
    emitEvent('phase_changed', { phase });
    emitEvent(`phase:${phase}`, { phase });
  }

  function waitForEvent<T = unknown>(name: string, options?: { timeoutMs?: number }): Promise<T> {
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
    });
  }

  aero.setPhase = setPhase;
  aero.emitEvent = emitEvent;
  aero.waitForEvent = waitForEvent;

  return { status, events, setPhase, emitEvent, waitForEvent };
}
