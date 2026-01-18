import { formatOneLineError } from '../text.js';
import { isInstanceOfSafe, tryGetErrorName } from './error_props';

export function isDataCloneError(err: unknown): boolean {
  const domException = (globalThis as unknown as { DOMException?: unknown }).DOMException;
  if (typeof domException === 'function') {
    if (isInstanceOfSafe(err, domException) && tryGetErrorName(err) === 'DataCloneError') return true;
  }

  if (tryGetErrorName(err) === 'DataCloneError') return true;

  const message = formatOneLineError(err, 2048);
  return /DataCloneError|could not be cloned/i.test(message);
}

export function isTier1AbiMismatchError(err: unknown): boolean {
  // wasm-bindgen argument mismatches typically show up as TypeErrors (wrong BigInt/number types,
  // wrong arg count, etc). Use a best-effort heuristic so we don't accidentally swallow real
  // compiler/runtime errors by retrying with legacy call signatures.
  if (isInstanceOfSafe(err, TypeError)) return true;
  const message = formatOneLineError(err, 2048);
  return /bigint|cannot convert|argument|parameter|is not a function/i.test(message);
}
