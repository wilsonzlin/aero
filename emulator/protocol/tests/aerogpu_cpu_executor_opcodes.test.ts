import assert from "node:assert/strict";
import test from "node:test";

import { AerogpuCmdOpcode } from "../aerogpu/aerogpu_cmd.ts";
import { aerogpuCpuExecutorSupportsOpcode } from "../../../web/src/workers/aerogpu-acmd-executor.ts";

test("aerogpuCpuExecutorSupportsOpcode matches expected minimal opcode coverage", () => {
  // Supported no-ops / forward-compat.
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.Nop), true);
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.DebugMarker), true);

  // Supported resource/present path.
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.CreateTexture2d), true);
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.UploadResource), true);
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.Present), true);
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.PresentEx), true);

  // Known unsupported opcodes should fall back to wasm/wgpu execution.
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.Draw), false);
  assert.equal(aerogpuCpuExecutorSupportsOpcode(AerogpuCmdOpcode.CreateShaderDxbc), false);

  // Unknown opcodes should also fall back to wasm/wgpu execution.
  assert.equal(aerogpuCpuExecutorSupportsOpcode(0xdeadbeef), false);
});

