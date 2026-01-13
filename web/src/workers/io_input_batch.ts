import { InputEventType, type InputEventType as InputEventTypeT } from "../input/event_queue";

export const INPUT_BATCH_HEADER_WORDS = 2;
export const INPUT_BATCH_WORDS_PER_EVENT = 4;
export const INPUT_BATCH_HEADER_BYTES = INPUT_BATCH_HEADER_WORDS * 4;

export type InputBatchValidationError =
  | "buffer_too_small"
  | "buffer_unaligned"
  | "count_out_of_bounds"
  | "unknown_event_type"
  | "invalid_scancode_len";

export type InputBatchValidationResult =
  | { ok: true; words: Int32Array; count: number }
  | { ok: false; error: InputBatchValidationError };

function isKnownInputEventType(v: number): v is InputEventTypeT {
  switch (v) {
    case InputEventType.KeyScancode:
    case InputEventType.KeyHidUsage:
    case InputEventType.MouseMove:
    case InputEventType.MouseButtons:
    case InputEventType.MouseWheel:
    case InputEventType.GamepadReport:
      return true;
    default:
      return false;
  }
}

/**
 * Validates an untrusted input batch `ArrayBuffer` and returns a safe view over it.
 *
 * This is intentionally strict: unknown event types or structurally invalid payloads
 * cause the entire batch to be rejected so the I/O worker never risks unbounded loops
 * or large allocations due to corrupted headers.
 */
export function validateInputBatchBuffer(buffer: ArrayBuffer): InputBatchValidationResult {
  const byteLength = buffer.byteLength >>> 0;
  if (byteLength < INPUT_BATCH_HEADER_BYTES) return { ok: false, error: "buffer_too_small" };
  if ((byteLength & 3) !== 0) return { ok: false, error: "buffer_unaligned" };

  // Safe after the alignment check above.
  const words = new Int32Array(buffer);
  if (words.length < INPUT_BATCH_HEADER_WORDS) return { ok: false, error: "buffer_too_small" };

  const count = words[0]! >>> 0;
  const maxEvents = ((words.length - INPUT_BATCH_HEADER_WORDS) / INPUT_BATCH_WORDS_PER_EVENT) | 0;
  if (count > maxEvents) return { ok: false, error: "count_out_of_bounds" };

  const base = INPUT_BATCH_HEADER_WORDS;
  for (let i = 0; i < count; i += 1) {
    const off = base + i * INPUT_BATCH_WORDS_PER_EVENT;
    const type = words[off]! >>> 0;
    if (!isKnownInputEventType(type)) return { ok: false, error: "unknown_event_type" };
    if (type === InputEventType.KeyScancode) {
      const len = words[off + 3]! >>> 0;
      // PS/2 set-2 scancodes are packed into a single u32 (1..4 bytes).
      if (len === 0 || len > 4) return { ok: false, error: "invalid_scancode_len" };
    }
  }

  return { ok: true, words, count };
}

