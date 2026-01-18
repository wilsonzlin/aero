import test from 'node:test';
import assert from 'node:assert/strict';

import { isDataCloneError, isTier1AbiMismatchError } from '../src/workers/jit_error_utils.ts';

test('jit_error_utils: isDataCloneError detects DataCloneError (DOMException + name)', () => {
  const original = globalThis.DOMException;
  try {
    globalThis.DOMException = class DOMException extends Error {
      constructor() {
        super('nope');
        this.name = 'DataCloneError';
      }
    };

    const err = new globalThis.DOMException();
    assert.equal(isDataCloneError(err), true);
  } finally {
    globalThis.DOMException = original;
  }
});

test('jit_error_utils: isDataCloneError detects DataCloneError (name only)', () => {
  assert.equal(isDataCloneError({ name: 'DataCloneError' }), true);
});

test('jit_error_utils: helpers do not throw on hostile error-like objects', () => {
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

  assert.doesNotThrow(() => isDataCloneError(hostileName));
  assert.doesNotThrow(() => isDataCloneError(hostileProto));
  assert.doesNotThrow(() => isTier1AbiMismatchError(hostileProto));
});

test('jit_error_utils: isTier1AbiMismatchError treats TypeError as mismatch', () => {
  assert.equal(isTier1AbiMismatchError(new TypeError('wrong arg count')), true);
});
