let fallbackCtor = null;

export function createCustomEvent(type, detail) {
  const CE = globalThis.CustomEvent;
  if (typeof CE === 'function') {
    try {
      return new CE(type, { detail });
    } catch {
      // Fall through to our safe Event-based implementation.
    }
  }

  if (!fallbackCtor) {
    const EventCtor = globalThis.Event;
    if (typeof EventCtor !== 'function') {
      throw new Error('createCustomEvent: Event is unavailable');
    }
    fallbackCtor = class CustomEventFallback extends EventCtor {
      constructor(type, init = {}) {
        super(type);
        this.detail = init.detail;
      }
    };
  }

  return new fallbackCtor(type, { detail });
}
