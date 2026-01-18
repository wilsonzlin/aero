import test from 'node:test';
import assert from 'node:assert/strict';

import { serializeWebUsbProbeError } from '../src/workers/webusb_probe_error_utils.ts';

test('webusb_probe_error_utils: serializes DOMException-like errors', () => {
  const original = globalThis.DOMException;
  try {
    globalThis.DOMException = class DOMException extends Error {
      constructor() {
        super('denied');
        this.name = 'NotAllowedError';
      }
    };

    const err = new globalThis.DOMException();
    const out = serializeWebUsbProbeError(err);
    assert.equal(out.name, 'NotAllowedError');
    assert.equal(out.message, 'denied');
  } finally {
    globalThis.DOMException = original;
  }
});

test('webusb_probe_error_utils: does not throw on hostile errors', () => {
  const hostileName = {};
  Object.defineProperty(hostileName, 'name', {
    get() {
      throw new Error('boom');
    },
  });

  const hostileProto = new Proxy(
    {},
    {
      getPrototypeOf() {
        throw new Error('boom');
      },
    },
  );

  assert.doesNotThrow(() => serializeWebUsbProbeError(hostileName));
  assert.doesNotThrow(() => serializeWebUsbProbeError(hostileProto));
});
