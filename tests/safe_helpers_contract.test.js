import test from 'node:test';
import assert from 'node:assert/strict';

import { isInstanceOfSafe } from '../src/instanceof_safe.js';
import { tryGetNumberProp, tryGetProp, tryGetStringProp } from '../src/safe_props.js';
import safePropsCjs from '../src/safe_props.cjs';

test('safe_props: getters are safe against hostile objects', () => {
  const hostileGetter = {};
  Object.defineProperty(hostileGetter, 'x', {
    get() {
      throw new Error('boom');
    },
  });

  const hostileProxy = new Proxy(
    {},
    {
      get() {
        throw new Error('boom');
      },
    },
  );

  assert.doesNotThrow(() => tryGetProp(hostileGetter, 'x'));
  assert.equal(tryGetProp(hostileGetter, 'x'), undefined);

  assert.doesNotThrow(() => tryGetProp(hostileProxy, 'x'));
  assert.equal(tryGetProp(hostileProxy, 'x'), undefined);
});

test('safe_props: CJS parity', () => {
  assert.equal(typeof safePropsCjs.tryGetProp, 'function');
  assert.equal(typeof safePropsCjs.tryGetStringProp, 'function');
  assert.equal(typeof safePropsCjs.tryGetNumberProp, 'function');

  const hostileGetter = {};
  Object.defineProperty(hostileGetter, 'x', {
    get() {
      throw new Error('boom');
    },
  });

  const cases = [
    [{ a: 'x' }, 'a'],
    [{ a: 1 }, 'a'],
    [hostileGetter, 'x'],
    [null, 'a'],
    [undefined, 'a'],
  ];

  for (const [obj, key] of cases) {
    assert.equal(tryGetProp(obj, key), safePropsCjs.tryGetProp(obj, key));
    assert.equal(tryGetStringProp(obj, key), safePropsCjs.tryGetStringProp(obj, key));
    assert.equal(tryGetNumberProp(obj, key), safePropsCjs.tryGetNumberProp(obj, key));
  }
});

test('safe_props: typed helpers filter correctly', () => {
  assert.equal(tryGetStringProp({ a: 'x' }, 'a'), 'x');
  assert.equal(tryGetStringProp({ a: 1 }, 'a'), undefined);

  assert.equal(tryGetNumberProp({ n: 1 }, 'n'), 1);
  assert.equal(tryGetNumberProp({ n: NaN }, 'n'), undefined);
  assert.equal(tryGetNumberProp({ n: Infinity }, 'n'), undefined);
});

test('safe_props: supports symbol keys', () => {
  const sym = Symbol('x');
  const obj = { [sym]: 'ok' };
  assert.equal(tryGetProp(obj, sym), 'ok');
  assert.equal(tryGetStringProp(obj, sym), 'ok');
  assert.equal(tryGetNumberProp(obj, sym), undefined);

  assert.equal(safePropsCjs.tryGetProp(obj, sym), 'ok');
  assert.equal(safePropsCjs.tryGetStringProp(obj, sym), 'ok');
  assert.equal(safePropsCjs.tryGetNumberProp(obj, sym), undefined);
});

test('instanceof_safe: never throws and returns false on hostile proxies', () => {
  const hostile = new Proxy(
    {},
    {
      getPrototypeOf() {
        throw new Error('boom');
      },
    },
  );

  assert.doesNotThrow(() => isInstanceOfSafe(hostile, Error));
  assert.equal(isInstanceOfSafe(hostile, Error), false);
  assert.equal(isInstanceOfSafe(new Error('x'), Error), true);
  assert.equal(isInstanceOfSafe(null, Error), false);
});
