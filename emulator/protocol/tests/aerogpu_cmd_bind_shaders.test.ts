import assert from "node:assert/strict";
import test from "node:test";

import { AerogpuCmdOpcode, decodeCmdBindShadersPayload } from "../aerogpu/aerogpu_cmd.ts";

test("decodeCmdBindShadersPayload decodes base packet", () => {
  const bytes = new Uint8Array(24);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(0, AerogpuCmdOpcode.BindShaders, true);
  view.setUint32(4, bytes.byteLength, true);
  view.setUint32(8, 1, true); // vs
  view.setUint32(12, 2, true); // ps
  view.setUint32(16, 3, true); // cs
  view.setUint32(20, 0xaabbccdd, true); // reserved0

  const decoded = decodeCmdBindShadersPayload(bytes, 0);
  assert.equal(decoded.vs, 1);
  assert.equal(decoded.ps, 2);
  assert.equal(decoded.cs, 3);
  assert.equal(decoded.reserved0, 0xaabbccdd >>> 0);
  assert.equal(decoded.ex, undefined);
});

test("decodeCmdBindShadersPayload ignores unknown trailing bytes in base packets", () => {
  // Payload is 20 bytes: base 16 + 4 bytes of forward-compatible extension (not enough for the
  // `{gs,hs,ds}` extension table, so `ex` remains absent).
  const bytes = new Uint8Array(28);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(0, AerogpuCmdOpcode.BindShaders, true);
  view.setUint32(4, bytes.byteLength, true);
  view.setUint32(8, 1, true); // vs
  view.setUint32(12, 2, true); // ps
  view.setUint32(16, 3, true); // cs
  view.setUint32(20, 0xaabbccdd, true); // reserved0
  view.setUint32(24, 0xdeadbeef, true); // trailing extension (ignored)

  const decoded = decodeCmdBindShadersPayload(bytes, 0);
  assert.equal(decoded.vs, 1);
  assert.equal(decoded.ps, 2);
  assert.equal(decoded.cs, 3);
  assert.equal(decoded.reserved0, 0xaabbccdd >>> 0);
  assert.equal(decoded.ex, undefined);
});

test("decodeCmdBindShadersPayload decodes extended packet", () => {
  const bytes = new Uint8Array(36);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(0, AerogpuCmdOpcode.BindShaders, true);
  view.setUint32(4, bytes.byteLength, true);
  view.setUint32(8, 1, true); // vs
  view.setUint32(12, 2, true); // ps
  view.setUint32(16, 3, true); // cs
  view.setUint32(20, 0, true); // reserved0
  view.setUint32(24, 4, true); // gs
  view.setUint32(28, 5, true); // hs
  view.setUint32(32, 6, true); // ds

  const decoded = decodeCmdBindShadersPayload(bytes, 0);
  assert.deepEqual(decoded.ex, { gs: 4, hs: 5, ds: 6 });
});

test("decodeCmdBindShadersPayload ignores trailing bytes after extended handles", () => {
  const bytes = new Uint8Array(40);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(0, AerogpuCmdOpcode.BindShaders, true);
  view.setUint32(4, bytes.byteLength, true);
  view.setUint32(8, 1, true); // vs
  view.setUint32(12, 2, true); // ps
  view.setUint32(16, 3, true); // cs
  view.setUint32(20, 0, true); // reserved0
  view.setUint32(24, 4, true); // gs
  view.setUint32(28, 5, true); // hs
  view.setUint32(32, 6, true); // ds
  view.setUint32(36, 0xdeadbeef, true); // trailing extension (ignored)

  const decoded = decodeCmdBindShadersPayload(bytes, 0);
  assert.deepEqual(decoded.ex, { gs: 4, hs: 5, ds: 6 });
});

