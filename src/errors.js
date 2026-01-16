import { formatBytes } from './utils.js';
import { formatOneLineUtf8, truncateUtf8 } from './text.js';

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
  if (err && typeof err === 'object' && err.name === 'EmulatorError' && typeof err.code === 'string') {
    const name = formatOneLineUtf8(err.name, MAX_ERROR_NAME_BYTES) || 'Error';
    const message = formatOneLineUtf8(err.message, MAX_ERROR_MESSAGE_BYTES) || 'Error';
    const stack = typeof err.stack === 'string' ? truncateUtf8(err.stack, MAX_ERROR_STACK_BYTES) : undefined;
    return {
      name,
      code: err.code,
      message,
      details: err.details,
      suggestion: err.suggestion,
      ...(stack ? { stack } : {}),
    };
  }

  if (err instanceof Error) {
    const name = formatOneLineUtf8(err.name, MAX_ERROR_NAME_BYTES) || 'Error';
    const messageRaw = typeof fallback.message === 'string' ? fallback.message : err.message;
    const message = formatOneLineUtf8(messageRaw, MAX_ERROR_MESSAGE_BYTES) || 'Error';
    const stack = typeof err.stack === 'string' ? truncateUtf8(err.stack, MAX_ERROR_STACK_BYTES) : undefined;
    return {
      name,
      code: fallback.code ?? ErrorCode.InternalError,
      message,
      details: fallback.details,
      suggestion: fallback.suggestion,
      ...(stack ? { stack } : {}),
    };
  }

  const messageRaw = typeof fallback.message === 'string' ? fallback.message : String(err);
  const message = formatOneLineUtf8(messageRaw, MAX_ERROR_MESSAGE_BYTES) || 'Error';
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

