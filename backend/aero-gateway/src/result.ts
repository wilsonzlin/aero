export type ErrorCode =
  | 'INVALID_TARGET'
  | 'INVALID_HOST'
  | 'INVALID_PORT'
  | 'DISALLOWED_PORT'
  | 'PRIVATE_IP_BLOCKED'
  | 'HOST_NOT_ALLOWED'
  | 'FRAME_TRUNCATED'
  | 'FRAME_TOO_LARGE'
  | 'DNS_PARAM_INVALID_BASE64URL'
  | 'DNS_PARAM_TOO_LARGE'
  | 'INTERNAL_ERROR';

export type Result<T> = { ok: true; value: T } | { ok: false; code: ErrorCode; message: string };

export function ok<T>(value: T): Result<T> {
  return { ok: true, value };
}

export function err<T = never>(code: ErrorCode, message: string): Result<T> {
  return { ok: false, code, message };
}

export function safeResult<T>(fn: () => Result<T>): Result<T> {
  try {
    return fn();
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return err('INTERNAL_ERROR', message);
  }
}

