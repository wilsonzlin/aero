import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AEROGPU_STAGE_EX_MIN_ABI_MINOR,
  AerogpuCmdWriter,
  AerogpuShaderStage,
  AerogpuShaderStageEx,
  decodeCmdCreateShaderDxbcPayload,
  decodeStageExGated,
  resolveStageEx,
  resolveShaderStageWithExGated,
} from "../aerogpu/aerogpu_cmd.ts";

test("legacy compute packets (shader_stage=COMPUTE, reserved0=0) resolve to Compute", () => {
  const w = new AerogpuCmdWriter();
  w.createShaderDxbc(1, AerogpuShaderStage.Compute, new Uint8Array([]));
  const stream = w.finish();

  const decoded = decodeCmdCreateShaderDxbcPayload(stream, AEROGPU_CMD_STREAM_HEADER_SIZE);
  assert.equal(decoded.stage, AerogpuShaderStage.Compute);
  assert.equal(decoded.reserved0, 0);
  assert.equal(resolveStageEx(decoded.stage, decoded.reserved0), "compute");
});

test("extended stage_ex packets (shader_stage=COMPUTE, reserved0!=0) resolve GS/HS/DS", () => {
  const cases: Array<[AerogpuShaderStageEx, string]> = [
    [AerogpuShaderStageEx.Geometry, "geometry"],
    [AerogpuShaderStageEx.Hull, "hull"],
    [AerogpuShaderStageEx.Domain, "domain"],
  ];

  for (const [stageEx, expected] of cases) {
    const w = new AerogpuCmdWriter();
    w.createShaderDxbc(1, AerogpuShaderStage.Compute, new Uint8Array([]), stageEx);
    const stream = w.finish();

    const decoded = decodeCmdCreateShaderDxbcPayload(stream, AEROGPU_CMD_STREAM_HEADER_SIZE);
    assert.equal(decoded.stage, AerogpuShaderStage.Compute);
    assert.equal(decoded.reserved0, stageEx);
    assert.equal(resolveStageEx(decoded.stage, decoded.reserved0), expected);
  }
});

test("AerogpuShaderStageEx intentionally omits Vertex=1 (DXBC program type) and treats it as invalid", () => {
  // stage_ex uses DXBC program type numbers, but Pixel (0) and Vertex (1) must be encoded via the
  // legacy `shader_stage` field for clarity.
  assert.ok(!("Vertex" in AerogpuShaderStageEx));
  assert.equal((AerogpuShaderStageEx as unknown as Record<string, unknown>).Vertex, undefined);
});

test("stage_ex helpers are gated by command stream ABI minor", () => {
  // Pre-stage_ex streams (ABI minor < AEROGPU_STAGE_EX_MIN_ABI_MINOR) must ignore reserved0 when
  // shader_stage==Compute.
  assert.equal(
    decodeStageExGated(AEROGPU_STAGE_EX_MIN_ABI_MINOR - 1, AerogpuShaderStage.Compute, AerogpuShaderStageEx.Geometry),
    AerogpuShaderStageEx.None,
  );
  assert.deepEqual(
    resolveShaderStageWithExGated(
      AEROGPU_STAGE_EX_MIN_ABI_MINOR - 1,
      AerogpuShaderStage.Compute,
      AerogpuShaderStageEx.Geometry,
    ),
    { kind: "Compute" },
  );

  // Ensure other extended stages are also gated.
  assert.equal(
    decodeStageExGated(AEROGPU_STAGE_EX_MIN_ABI_MINOR - 1, AerogpuShaderStage.Compute, AerogpuShaderStageEx.Hull),
    AerogpuShaderStageEx.None,
  );
  assert.equal(
    decodeStageExGated(AEROGPU_STAGE_EX_MIN_ABI_MINOR - 1, AerogpuShaderStage.Compute, AerogpuShaderStageEx.Domain),
    AerogpuShaderStageEx.None,
  );

  // ABI minors at/after the stage_ex introduction must honor stage_ex.
  assert.equal(
    decodeStageExGated(AEROGPU_STAGE_EX_MIN_ABI_MINOR, AerogpuShaderStage.Compute, AerogpuShaderStageEx.Geometry),
    AerogpuShaderStageEx.Geometry,
  );
  assert.deepEqual(
    resolveShaderStageWithExGated(
      AEROGPU_STAGE_EX_MIN_ABI_MINOR,
      AerogpuShaderStage.Compute,
      AerogpuShaderStageEx.Geometry,
    ),
    { kind: "Geometry" },
  );

  assert.equal(
    decodeStageExGated(AEROGPU_STAGE_EX_MIN_ABI_MINOR, AerogpuShaderStage.Compute, AerogpuShaderStageEx.Hull),
    AerogpuShaderStageEx.Hull,
  );
  assert.equal(
    decodeStageExGated(AEROGPU_STAGE_EX_MIN_ABI_MINOR, AerogpuShaderStage.Compute, AerogpuShaderStageEx.Domain),
    AerogpuShaderStageEx.Domain,
  );

  // stage_ex decoding is only active when shaderStage == Compute.
  assert.equal(decodeStageExGated(0, AerogpuShaderStage.Pixel, AerogpuShaderStageEx.Hull), undefined);
  assert.deepEqual(resolveShaderStageWithExGated(0, AerogpuShaderStage.Pixel, AerogpuShaderStageEx.Hull), {
    kind: "Pixel",
  });
});
