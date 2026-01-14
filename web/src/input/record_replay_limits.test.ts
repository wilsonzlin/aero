import { describe, expect, it } from "vitest";

import { InputRecordReplay, u32WordsToInputBatch } from "./record_replay";

describe("InputRecordReplay limits", () => {
  it("skips imported batches whose word arrays are unreasonably large", () => {
    const recorder = new InputRecordReplay();

    const hugeWords = new Array<number>(2_000_000); // > 4MiB worth of u32 words (cap is 1_048_576).

    recorder.importJson({
      version: 1,
      batches: [{ words: hugeWords }, { words: [0, 1] }],
    });

    expect(recorder.size).toBe(1);
    const buf = recorder.cloneBatchBuffer(0);
    expect(Array.from(new Uint32Array(buf))).toEqual([0, 1]);
  });

  it("rejects converting absurd u32 word arrays into an input batch buffer", () => {
    const hugeWords = new Array<number>(2_000_000);
    expect(() => u32WordsToInputBatch(hugeWords)).toThrow(/exceeds max/i);
  });
});
