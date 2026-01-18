import { formatBytes } from './utils.js';
import { isInstanceOfSafe } from './instanceof_safe.js';
import { tryGetStringProp } from './safe_props.js';
import { formatOneLineError, formatOneLineUtf8, truncateUtf8 } from './text.js';

const MAX_ERROR_NAME_BYTES = 128;
const MAX_ERROR_MESSAGE_BYTES = 512;
const MAX_ERROR_STACK_BYTES = 8 * 1024;

export const ErrorCode = Object.freeze({
  WatchdogTimeout: 'WatchdogTimeout',
  WorkerCrashed: 'WorkerCrashed',
  ResourceLimitExceeded: 'ResourceLimitExceeded',
  OutOfMemory: 'OutOfMemory',
  InvalidConfig: 'InvalidConfig',
  InternalError: 'InternalError',
});

export class EmulatorError extends Error {
  constructor(code, message, { details, suggestion, cause } = {}) {
    super(message);
    this.name = 'EmulatorError';
    this.code = code;
    this.details = details;
    this.suggestion = suggestion;
    this.cause = cause;
  }
}

export function serializeError(err, fallback = {}) {
  if (isInstanceOfSafe(err, EmulatorError)) {
    const name = formatOneLineUtf8(tryGetStringProp(err, 'name') ?? 'EmulatorError', MAX_ERROR_NAME_BYTES) || 'Error';
    const message = formatOneLineError(err, MAX_ERROR_MESSAGE_BYTES);
    const stackRaw = tryGetStringProp(err, 'stack');
    const stack = stackRaw ? truncateUtf8(stackRaw, MAX_ERROR_STACK_BYTES) : undefined;
    return {
      name,
      code: err.code,
      message,
      details: err.details,
      suggestion: err.suggestion,
      ...(stack ? { stack } : {}),
    };
  }

  if (isInstanceOfSafe(err, Error)) {
    const name = formatOneLineUtf8(tryGetStringProp(err, 'name') ?? 'Error', MAX_ERROR_NAME_BYTES) || 'Error';
    const message =
      typeof fallback.message === 'string'
        ? formatOneLineUtf8(fallback.message, MAX_ERROR_MESSAGE_BYTES) || 'Error'
        : formatOneLineError(err, MAX_ERROR_MESSAGE_BYTES);
    const stackRaw = tryGetStringProp(err, 'stack');
    const stack = stackRaw ? truncateUtf8(stackRaw, MAX_ERROR_STACK_BYTES) : undefined;
    return {
      name,
      code: fallback.code ?? ErrorCode.InternalError,
      message,
      details: fallback.details,
      suggestion: fallback.suggestion,
      ...(stack ? { stack } : {}),
    };
  }

  const message =
    typeof fallback.message === 'string'
      ? formatOneLineUtf8(fallback.message, MAX_ERROR_MESSAGE_BYTES) || 'Error'
      : formatOneLineError(err, MAX_ERROR_MESSAGE_BYTES);
  return {
    name: 'Error',
    code: fallback.code ?? ErrorCode.InternalError,
    message,
    details: fallback.details,
    suggestion: fallback.suggestion,
  };
}

export function resourceLimitExceeded({ resource, requestedBytes, maxBytes }) {
  const requested = formatBytes(requestedBytes);
  const max = formatBytes(maxBytes);
  return new EmulatorError(
    ErrorCode.ResourceLimitExceeded,
    `${resource} request (${requested}) exceeds configured maximum (${max}).`,
    {
      details: { resource, requestedBytes, maxBytes },
      suggestion: `Reduce ${resource} usage or increase the configured maximum.`,
    },
  );
}

export function outOfMemory({ resource, attemptedBytes, cause }) {
  const attempted = formatBytes(attemptedBytes);
  return new EmulatorError(
    ErrorCode.OutOfMemory,
    `Unable to allocate ${attempted} for ${resource}.`,
    {
      details: { resource, attemptedBytes },
      suggestion: `Try lowering ${resource}, closing other tabs, or using a 64-bit browser.`,
      cause,
    },
  );
}

