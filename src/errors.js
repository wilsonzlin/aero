import { formatBytes } from './utils.js';

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
    return {
      name: err.name,
      code: err.code,
      message: err.message,
      details: err.details,
      suggestion: err.suggestion,
      stack: err.stack,
    };
  }

  if (err instanceof Error) {
    return {
      name: err.name,
      code: fallback.code ?? ErrorCode.InternalError,
      message: fallback.message ?? err.message,
      details: fallback.details,
      suggestion: fallback.suggestion,
      stack: err.stack,
    };
  }

  return {
    name: 'Error',
    code: fallback.code ?? ErrorCode.InternalError,
    message: fallback.message ?? String(err),
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

