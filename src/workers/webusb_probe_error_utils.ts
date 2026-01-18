import { formatOneLineError, formatOneLineUtf8 } from '../text.js';
import { isInstanceOfSafe, tryGetErrorName } from './error_props';

const MAX_ERROR_NAME_BYTES = 128;
const MAX_ERROR_MESSAGE_BYTES = 512;

export function serializeWebUsbProbeError(err: unknown): { name: string; message: string } {
  const domException = (globalThis as unknown as { DOMException?: unknown }).DOMException;
  if (isInstanceOfSafe(err, domException)) {
    const name = formatOneLineUtf8(tryGetErrorName(err), MAX_ERROR_NAME_BYTES) || 'Error';
    const message = formatOneLineError(err, MAX_ERROR_MESSAGE_BYTES);
    return { name, message };
  }

  if (isInstanceOfSafe(err, Error)) {
    const name = formatOneLineUtf8(tryGetErrorName(err), MAX_ERROR_NAME_BYTES) || 'Error';
    const message = formatOneLineError(err, MAX_ERROR_MESSAGE_BYTES);
    return { name, message };
  }

  const safeName = formatOneLineUtf8(tryGetErrorName(err) ?? 'Error', MAX_ERROR_NAME_BYTES) || 'Error';
  const safeMessage = formatOneLineError(err, MAX_ERROR_MESSAGE_BYTES);
  return { name: safeName, message: safeMessage };
}
