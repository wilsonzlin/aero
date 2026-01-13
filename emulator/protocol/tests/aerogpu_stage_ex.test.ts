import assert from "node:assert/strict";
import test from "node:test";

import {
  AEROGPU_CMD_STREAM_HEADER_SIZE,
  AerogpuCmdWriter,
  AerogpuShaderStage,
  AerogpuShaderStageEx,
  decodeCmdCreateShaderDxbcPayload,
  resolveStageEx,
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

