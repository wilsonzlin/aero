import assert from "node:assert/strict";
import test from "node:test";

import { socketWritableLengthExceedsCap, socketWritableLengthOrOverflow } from "../src/socket_writable_length.js";
import socketWritableLengthCjs from "../src/socket_writable_length.cjs";

test("socket_writable_length: CJS parity", () => {
  assert.equal(typeof socketWritableLengthCjs.socketWritableLengthOrOverflow, "function");
  assert.equal(typeof socketWritableLengthCjs.socketWritableLengthExceedsCap, "function");

  const sockets = [
    { writableLength: 0 },
    { writableLength: 5 },
    {
      get writableLength() {
        throw new Error("boom");
      },
    },
    /** @type {any} */ ({ writableLength: "nope" }),
  ];

  for (const socket of sockets) {
    for (const cap of [0, 4, 5, 6, NaN]) {
      assert.equal(
        socketWritableLengthOrOverflow(socket, cap),
        socketWritableLengthCjs.socketWritableLengthOrOverflow(socket, cap),
      );
      assert.equal(
        socketWritableLengthExceedsCap(socket, cap),
        socketWritableLengthCjs.socketWritableLengthExceedsCap(socket, cap),
      );
    }
  }
});

test("socket_writable_length: socketWritableLengthExceedsCap fails closed on hostile getter", () => {
  const socket = {
    get writableLength() {
      throw new Error("boom");
    },
  };
  assert.equal(socketWritableLengthExceedsCap(socket, 10), true);
});

test("socket_writable_length: socketWritableLengthExceedsCap treats invalid caps as exceeded", () => {
  const socket = { writableLength: 0 };
  assert.equal(socketWritableLengthExceedsCap(socket, NaN), true);
  assert.equal(socketWritableLengthExceedsCap(socket, -1), true);
  assert.equal(socketWritableLengthExceedsCap(socket, Infinity), true);
  assert.equal(socketWritableLengthExceedsCap(socket, /** @type {any} */ ("nope")), true);
});

