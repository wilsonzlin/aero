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

import { formatOneLineError, formatOneLineUtf8 } from "./text";

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
    const safeMessage = formatOneLineError(err, MAX_LOG_ERROR_MESSAGE_BYTES);
    let rawName = "Error";
    try {
      if (typeof err.name === "string") rawName = err.name;
    } catch {
      // ignore getters throwing
    }
    const safeName = formatOneLineUtf8(rawName, 128) || "Error";
    return { name: safeName, message: safeMessage, code: (err as { code?: unknown }).code };
  }
  return { message: formatOneLineError(err, MAX_LOG_ERROR_MESSAGE_BYTES) };
}
