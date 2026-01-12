import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  HEADER_BYTES,
  HEADER_U32_LEN,
  OVERRUN_COUNT_INDEX,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
} from "./audio_worklet_ring";

function parseRustUsizeConst(source: string, name: string): number {
  // Keep the matcher intentionally strict so we fail loudly if the Rust source changes.
  const re = new RegExp(String.raw`^pub const ${name}: [^=]+ = (\d+);$`, "m");
  const match = source.match(re);
  if (!match) {
    throw new Error(`Failed to locate \`pub const ${name}\` in crates/platform/src/audio/worklet_bridge.rs`);
  }
  const value = Number(match[1]);
  if (!Number.isFinite(value)) {
    throw new Error(`Invalid numeric value for ${name}: ${match[1]}`);
  }
  return value;
}

describe("AudioWorklet ring layout matches Rust source of truth", () => {
  it("keeps playback ring header indices in sync with crates/platform/src/audio/worklet_bridge.rs", () => {
    const rustUrl = new URL("../../../crates/platform/src/audio/worklet_bridge.rs", import.meta.url);
    const rust = readFileSync(rustUrl, "utf8");

    const rustHeaderU32Len = parseRustUsizeConst(rust, "HEADER_U32_LEN");
    expect(HEADER_U32_LEN).toBe(rustHeaderU32Len);
    expect(HEADER_BYTES).toBe(rustHeaderU32Len * 4);
    expect(READ_FRAME_INDEX).toBe(parseRustUsizeConst(rust, "READ_FRAME_INDEX"));
    expect(WRITE_FRAME_INDEX).toBe(parseRustUsizeConst(rust, "WRITE_FRAME_INDEX"));
    expect(UNDERRUN_COUNT_INDEX).toBe(parseRustUsizeConst(rust, "UNDERRUN_COUNT_INDEX"));
    expect(OVERRUN_COUNT_INDEX).toBe(parseRustUsizeConst(rust, "OVERRUN_COUNT_INDEX"));
  });
});
