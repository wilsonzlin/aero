import type { InputBatchMessage, InputBatchTarget, InputBatchFlushHook } from "./event_queue";
import { INPUT_BATCH_HEADER_BYTES } from "./event_queue";

export type InputBatchJsonWords = number[];

export type InputRecordReplayJsonV1 = {
  version: 1;
  /**
   * Sequence of recorded `in:input-batch` payloads.
   *
   * Each entry stores the entire transferred ArrayBuffer as u32 words so it is:
   * - deterministic (bit-exact),
   * - JSON-safe (no binary),
   * - independent of host endianness (wire format is little-endian u32/i32 words).
   */
  batches: Array<{
    words: InputBatchJsonWords;
    recycle?: true;
  }>;
};

export type InputRecordReplayJson = InputRecordReplayJsonV1;

type RecordedBatch = {
  words: Uint32Array;
  recycle: boolean;
};

// Memory safety: recorded batches may be imported from untrusted JSON (e.g. console tooling). Cap
// per-batch and total recorded size to avoid OOM from malformed input.
const MAX_RECORD_REPLAY_BATCH_BYTES = 4 * 1024 * 1024;
const MAX_RECORD_REPLAY_TOTAL_BYTES = 64 * 1024 * 1024;

export function inputBatchToU32Words(buffer: ArrayBuffer): InputBatchJsonWords {
  if (buffer.byteLength % 4 !== 0) {
    throw new Error(`Input batch buffer byteLength ${buffer.byteLength} is not a multiple of 4`);
  }
  if (buffer.byteLength < INPUT_BATCH_HEADER_BYTES) {
    throw new Error(`Input batch buffer byteLength ${buffer.byteLength} is too small`);
  }
  if (buffer.byteLength > MAX_RECORD_REPLAY_BATCH_BYTES) {
    throw new Error(`Input batch buffer byteLength ${buffer.byteLength} exceeds max ${MAX_RECORD_REPLAY_BATCH_BYTES}`);
  }
  // Preserve exact bits by reinterpreting as u32 words.
  return Array.from(new Uint32Array(buffer));
}

export function u32WordsToInputBatch(words: readonly number[]): ArrayBuffer {
  if (words.length > MAX_RECORD_REPLAY_BATCH_BYTES / 4) {
    throw new Error(`Input batch words length ${words.length} exceeds max ${(MAX_RECORD_REPLAY_BATCH_BYTES / 4) | 0}`);
  }
  const buf = new ArrayBuffer(words.length * 4);
  const view = new Uint32Array(buf);
  for (let i = 0; i < words.length; i++) {
    // Coerce to u32 to be resilient to JSON parsers producing non-int numbers.
    view[i] = (words[i] ?? 0) >>> 0;
  }
  return buf;
}

export class InputRecordReplay {
  private readonly batches: RecordedBatch[] = [];
  private recording = false;
  private totalBytes = 0;

  /**
   * A stable hook suitable for wiring directly into `InputEventQueue.flush({ onBeforeSend })`.
   *
   * When recording is inactive, this does not allocate.
   */
  readonly captureHook: InputBatchFlushHook = (_buffer, words, _count, recycle) => {
    if (!this.recording) {
      return;
    }

    // Copy the entire transferred buffer (including any unused trailing words)
    // to make "read past count" bugs reproducible.
    const byteLength = words.byteLength;
    if (byteLength > MAX_RECORD_REPLAY_BATCH_BYTES) {
      // Defensive: avoid unbounded allocations if a caller ever uses InputEventQueue with
      // a very large backing buffer (or provides a malicious hook invocation).
      return;
    }
    const copy = new Uint32Array(words.length);
    // `set(Int32Array)` performs numeric conversion, but preserves bit patterns:
    // -1 -> 0xFFFF_FFFF, etc.
    copy.set(words);
    this.batches.push({ words: copy, recycle });
    this.totalBytes += byteLength;
    while (this.totalBytes > MAX_RECORD_REPLAY_TOTAL_BYTES && this.batches.length > 0) {
      const dropped = this.batches.shift();
      if (!dropped) break;
      this.totalBytes -= dropped.words.byteLength;
    }
  };

  startRecording(): void {
    this.batches.length = 0;
    this.totalBytes = 0;
    this.recording = true;
  }

  stopRecording(): void {
    this.recording = false;
  }

  get isRecording(): boolean {
    return this.recording;
  }

  clear(): void {
    this.batches.length = 0;
    this.totalBytes = 0;
  }

  get size(): number {
    return this.batches.length;
  }

  exportJson(): InputRecordReplayJsonV1 {
    return {
      version: 1,
      batches: this.batches.map((b) => ({
        words: Array.from(b.words),
        recycle: b.recycle ? true : undefined,
      })),
    };
  }

  importJson(json: InputRecordReplayJson): void {
    if (!json || typeof json !== "object") {
      throw new Error("InputRecordReplay.importJson: expected object");
    }
    if ((json as { version?: unknown }).version !== 1) {
      throw new Error(`InputRecordReplay.importJson: unsupported version ${(json as { version?: unknown }).version}`);
    }

    const batches = (json as InputRecordReplayJsonV1).batches;
    if (!Array.isArray(batches)) {
      throw new Error("InputRecordReplay.importJson: expected batches[]");
    }

    this.batches.length = 0;
    this.totalBytes = 0;
    for (const entry of batches) {
      if (!entry || typeof entry !== "object") continue;
      const wordsJson = (entry as { words?: unknown }).words;
      if (!Array.isArray(wordsJson)) continue;
      if (wordsJson.length > MAX_RECORD_REPLAY_BATCH_BYTES / 4) continue;
      const words = new Uint32Array(wordsJson.length);
      for (let i = 0; i < wordsJson.length; i++) {
        words[i] = ((wordsJson[i] as number) ?? 0) >>> 0;
      }
      const recycle = (entry as { recycle?: unknown }).recycle === true;
      this.batches.push({ words, recycle });
      this.totalBytes += words.byteLength;
      while (this.totalBytes > MAX_RECORD_REPLAY_TOTAL_BYTES && this.batches.length > 0) {
        const dropped = this.batches.shift();
        if (!dropped) break;
        this.totalBytes -= dropped.words.byteLength;
      }
    }

    // Importing is an offline operation; do not implicitly start recording.
    this.recording = false;
  }

  cloneBatchBuffer(index: number): ArrayBuffer {
    const batch = this.batches[index];
    if (!batch) {
      throw new Error(`InputRecordReplay: batch ${index} does not exist (size=${this.batches.length})`);
    }
    const buf = new ArrayBuffer(batch.words.length * 4);
    new Uint32Array(buf).set(batch.words);
    return buf;
  }

  replay(target: InputBatchTarget, opts: { recycle?: boolean } = {}): void {
    for (const batch of this.batches) {
      const buffer = new ArrayBuffer(batch.words.length * 4);
      new Uint32Array(buffer).set(batch.words);
      const recycle = opts.recycle ?? batch.recycle;

      const msg: InputBatchMessage = recycle
        ? { type: "in:input-batch", buffer, recycle: true }
        : { type: "in:input-batch", buffer };
      target.postMessage(msg, [buffer]);
    }
  }
}

export const inputRecordReplay = new InputRecordReplay();

function isWindowGlobal(): boolean {
  return typeof (globalThis as any).document !== "undefined";
}

function ensureAeroGlobal(): any {
  const g = globalThis as any;
  if (!g.aero || typeof g.aero !== "object") g.aero = {};
  return g.aero;
}

/**
 * Installs `globalThis.aero.input.{startRecording,stopRecording,replay}` for manual debugging.
 *
 * Safe to call multiple times.
 */
export function installInputRecordReplayGlobalApi(recorder: InputRecordReplay = inputRecordReplay): void {
  if (!isWindowGlobal()) return;

  const aero = ensureAeroGlobal();
  const existing = aero.input;
  if (!existing || typeof existing !== "object") {
    aero.input = {};
  }

  const inputApi = aero.input as Record<string, unknown>;

  inputApi.startRecording = recorder.startRecording.bind(recorder);
  inputApi.stopRecording = () => {
    recorder.stopRecording();
    const json = recorder.exportJson();
    inputApi.lastRecording = json;
    return json;
  };
  inputApi.replay = (target: unknown, opts?: unknown) => {
    recorder.replay(target as InputBatchTarget, opts as { recycle?: boolean } | undefined);
  };
  inputApi.exportRecording = recorder.exportJson.bind(recorder);
  inputApi.importRecording = (json: unknown) => recorder.importJson(json as InputRecordReplayJson);
  inputApi.clearRecording = recorder.clear.bind(recorder);

  Object.defineProperty(inputApi, "recording", {
    enumerable: true,
    get: () => recorder.isRecording,
  });
  Object.defineProperty(inputApi, "size", {
    enumerable: true,
    get: () => recorder.size,
  });
}
