export type LogLevel = "debug" | "info" | "warn" | "error";

export type LogEvent =
  | "proxy_start"
  | "proxy_stop"
  | "connect_requested"
  | "connect_accepted"
  | "connect_denied"
  | "connect_error"
  | "conn_close"
  | "udp_drop_backpressure";

import { formatOneLineUtf8 } from "./text";

const MAX_LOG_ERROR_MESSAGE_BYTES = 512;

export function log(level: LogLevel, event: LogEvent, fields: Record<string, unknown> = {}): void {
  const entry = {
    ts: new Date().toISOString(),
    level,
    event,
    ...fields
  };
  // JSONL structured logging.
  // eslint-disable-next-line no-console
  console.log(JSON.stringify(entry));
}

export function formatError(err: unknown): { message: string; name?: string; code?: unknown } {
  if (err instanceof Error) {
    // `code` is often non-standard (NodeJS.ErrnoException) but extremely useful.
    const safeMessage = formatOneLineUtf8(err.message, MAX_LOG_ERROR_MESSAGE_BYTES) || "Error";
    const safeName = formatOneLineUtf8(err.name, 128) || "Error";
    return { name: safeName, message: safeMessage, code: (err as { code?: unknown }).code };
  }
  const safeMessage = formatOneLineUtf8(String(err), MAX_LOG_ERROR_MESSAGE_BYTES) || "Error";
  return { message: safeMessage };
}
