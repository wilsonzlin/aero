import type { AeroPhase, AeroStatusSnapshot } from '../../../shared/aero_status.ts';

export interface AeroHostApi {
  status: AeroStatusSnapshot;
  events: EventTarget;
  setPhase(phase: AeroPhase): void;
  emitEvent(name: string, detail?: unknown): void;
  waitForEvent<T = unknown>(name: string, options?: { timeoutMs?: number }): Promise<T>;
}

declare global {
  interface Window {
    aero?: AeroHostApi;
  }
}

export function initAeroStatusApi(initialPhase: AeroPhase = 'booting'): AeroHostApi {
  if (window.aero) return window.aero;

  const events = new EventTarget();
  const status: AeroStatusSnapshot = {
    phase: initialPhase,
    phaseSinceMs: performance.now(),
  };

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

  window.aero = {
    status,
    events,
    setPhase,
    emitEvent,
    waitForEvent,
  };

  return window.aero;
}

