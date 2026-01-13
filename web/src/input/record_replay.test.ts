import { describe, expect, it } from "vitest";

import { InputEventQueue, type InputBatchTarget } from "./event_queue";
import { InputRecordReplay } from "./record_replay";

type DecodedInputEvent = {
  type: number;
  timestampUs: number;
  a: number;
  b: number;
};

function decodeEventStream(buffer: ArrayBuffer): DecodedInputEvent[] {
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;
  const events: DecodedInputEvent[] = [];
  const base = 2;
  for (let i = 0; i < count; i += 1) {
    const off = base + i * 4;
    events.push({
      type: words[off] >>> 0,
      timestampUs: words[off + 1] >>> 0,
      a: words[off + 2] | 0,
      b: words[off + 3] | 0,
    });
  }
  return events;
}

describe("InputRecordReplay", () => {
  it("roundtrips a recorded batch through JSON with identical Int32 words", () => {
    const queue = new InputEventQueue(8);
    queue.pushKeyScancode(10, 0xaa, 1);
    queue.pushKeyHidUsage(11, 0x04, true);
    queue.pushMouseMove(12, 5, -3);
    queue.pushMouseWheel(13, 1);

    const recorder = new InputRecordReplay();
    recorder.startRecording();

    const posted: ArrayBuffer[] = [];
    const target: InputBatchTarget = {
      postMessage: (msg) => {
        posted.push(msg.buffer);
      },
    };

    queue.flush(target, { recycle: false, onBeforeSend: recorder.captureHook });
    expect(posted).toHaveLength(1);
    expect(recorder.size).toBe(1);

    const json = recorder.exportJson();
    const parsed = JSON.parse(JSON.stringify(json));

    const restored = new InputRecordReplay();
    restored.importJson(parsed);

    const restoredBuffer = restored.cloneBatchBuffer(0);
    expect(Array.from(new Int32Array(restoredBuffer))).toEqual(Array.from(new Int32Array(posted[0]!)));
  });

  it("replays into a target producing the same decoded event stream", () => {
    const queue = new InputEventQueue(8);
    const recorder = new InputRecordReplay();
    recorder.startRecording();

    const postedBuffers: ArrayBuffer[] = [];
    const target: InputBatchTarget = {
      postMessage: (msg) => {
        postedBuffers.push(msg.buffer);
      },
    };

    // Batch 1
    queue.pushKeyScancode(10, 0xaa, 1);
    queue.pushMouseButtons(11, 1);
    queue.flush(target, { recycle: false, onBeforeSend: recorder.captureHook });

    // Batch 2
    queue.pushMouseMove(12, 3, -2);
    queue.pushMouseWheel(13, -1);
    queue.flush(target, { recycle: false, onBeforeSend: recorder.captureHook });

    const expectedStream = postedBuffers.flatMap((buf) => decodeEventStream(buf));

    const replayedStream: DecodedInputEvent[] = [];
    const replayTarget: InputBatchTarget = {
      postMessage: (msg) => {
        replayedStream.push(...decodeEventStream(msg.buffer));
      },
    };

    recorder.replay(replayTarget, { recycle: false });
    expect(replayedStream).toEqual(expectedStream);
  });
});

