import {
  INPUT_BATCH_HEADER_BYTES,
  INPUT_BATCH_HEADER_WORDS,
  INPUT_BATCH_WORDS_PER_EVENT,
  MAX_INPUT_EVENTS_PER_BATCH,
} from "../input/event_queue";

export {
  INPUT_BATCH_HEADER_BYTES,
  INPUT_BATCH_HEADER_WORDS,
  INPUT_BATCH_WORDS_PER_EVENT,
  MAX_INPUT_EVENTS_PER_BATCH,
};

export type InputBatchValidationError = "buffer_too_small" | "buffer_unaligned" | "int32_view_failed";

export type InputBatchValidationResult =
  | { ok: true; words: Int32Array; count: number; claimedCount: number; maxCount: number }
  | { ok: false; error: InputBatchValidationError };

/**
 * Validates an untrusted input batch `ArrayBuffer` and returns a safe view over it.
 *
 * This intentionally stays constant-time in the common case: we validate the buffer
 * length/alignment and clamp the claimed event count so consumers never loop based
 * on untrusted header values.
 */
export function validateInputBatchBuffer(buffer: ArrayBuffer): InputBatchValidationResult {
  const byteLength = buffer.byteLength >>> 0;
  if (byteLength < INPUT_BATCH_HEADER_BYTES) return { ok: false, error: "buffer_too_small" };
  if (byteLength % 4 !== 0) return { ok: false, error: "buffer_unaligned" };

  let words: Int32Array;
  try {
    words = new Int32Array(buffer);
  } catch {
    return { ok: false, error: "int32_view_failed" };
  }
  if (words.length < INPUT_BATCH_HEADER_WORDS) return { ok: false, error: "buffer_too_small" };

  const claimedCount = words[0] >>> 0;
  const maxCount = Math.floor((words.length - INPUT_BATCH_HEADER_WORDS) / INPUT_BATCH_WORDS_PER_EVENT);
  const count = Math.min(claimedCount, maxCount, MAX_INPUT_EVENTS_PER_BATCH);

  return { ok: true, words, count, claimedCount, maxCount };
}
