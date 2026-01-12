import assert from "node:assert/strict";
import test from "node:test";

import { AerogpuCmdWriter } from "../aerogpu/aerogpu_cmd.ts";
import { AerogpuFormat } from "../aerogpu/aerogpu_pci.ts";

import {
  createAerogpuCpuExecutorState,
  executeAerogpuCmdStream,
} from "../../../web/src/workers/aerogpu-acmd-executor.ts";

test("ACMD shared surface IMPORT creates alias handle usable for upload + present", () => {
  const state = createAerogpuCpuExecutorState();

  const token = 0x1234n;
  const upload = Uint8Array.from([
    // 2x2 RGBA8: pixel(0,0) pixel(1,0) pixel(0,1) pixel(1,1)
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
  ]);

  const w = new AerogpuCmdWriter();
  w.createTexture2d(1, 0, AerogpuFormat.R8G8B8A8Unorm, 2, 2, 1, 1, 0, 0, 0);
  w.exportSharedSurface(1, token);
  w.importSharedSurface(10, token);
  w.uploadResource(10, 0n, upload);
  w.setRenderTargets([10], 0);
  w.present(0, 0);

  executeAerogpuCmdStream(state, w.finish().buffer, { allocTable: null, guestU8: null });

  assert(state.lastPresentedFrame, "expected a present to populate lastPresentedFrame");
  assert.equal(state.lastPresentedFrame.width, 2);
  assert.equal(state.lastPresentedFrame.height, 2);
  assert.deepEqual(Array.from(new Uint8Array(state.lastPresentedFrame.rgba8)), Array.from(upload));
});

test("ACMD RELEASE_SHARED_SURFACE retires token but keeps existing imported handles valid", () => {
  const state = createAerogpuCpuExecutorState();

  const token = 0x9999n;
  const upload = Uint8Array.from([
    // 2x2 RGBA8
    31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46,
  ]);

  const setup = new AerogpuCmdWriter();
  setup.createTexture2d(1, 0, AerogpuFormat.R8G8B8A8Unorm, 2, 2, 1, 1, 0, 0, 0);
  setup.exportSharedSurface(1, token);
  setup.importSharedSurface(10, token);
  setup.uploadResource(10, 0n, upload);
  setup.releaseSharedSurface(token);
  executeAerogpuCmdStream(state, setup.finish().buffer, { allocTable: null, guestU8: null });

  // Token should no longer be importable.
  const importAgain = new AerogpuCmdWriter();
  importAgain.importSharedSurface(11, token);
  assert.throws(
    () => executeAerogpuCmdStream(state, importAgain.finish().buffer, { allocTable: null, guestU8: null }),
    /IMPORT_SHARED_SURFACE.*unknown share_token/i,
  );

  // Existing handles remain usable.
  const present = new AerogpuCmdWriter();
  present.setRenderTargets([10], 0);
  present.present(0, 0);
  executeAerogpuCmdStream(state, present.finish().buffer, { allocTable: null, guestU8: null });

  assert(state.lastPresentedFrame, "expected a present to populate lastPresentedFrame after release");
  assert.deepEqual(Array.from(new Uint8Array(state.lastPresentedFrame.rgba8)), Array.from(upload));
});

