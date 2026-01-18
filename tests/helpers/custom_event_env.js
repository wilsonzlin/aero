export function withCustomEventOverride(customEventValue, fn) {
  const originalDesc = Object.getOwnPropertyDescriptor(globalThis, 'CustomEvent');
  const hadOwn = originalDesc !== undefined;
  try {
    try {
      Object.defineProperty(globalThis, 'CustomEvent', {
        value: customEventValue,
        writable: true,
        enumerable: false,
        configurable: true,
      });
    } catch {
      try {
        globalThis.CustomEvent = customEventValue;
      } catch {
        // ignore
      }
    }
    return fn();
  } finally {
    try {
      if (hadOwn) {
        Object.defineProperty(globalThis, 'CustomEvent', originalDesc);
      } else {
        // Best-effort: if it wasn't an own property, remove our override.
        delete globalThis.CustomEvent;
      }
    } catch {
      // ignore
    }
  }
}
