import { formatOneLineUtf8, truncateUtf8 } from "../text";

export type ErrorByteLimits = {
  maxNameBytes: number;
  maxMessageBytes: number;
  maxStackBytes: number;
};

export type WorkerSerializedError = { message: string; name?: string; stack?: string };
export type ProtocolSerializedError = { name: string; message: string; stack?: string };

export const DEFAULT_ERROR_BYTE_LIMITS: ErrorByteLimits = Object.freeze({
  maxNameBytes: 128,
  maxMessageBytes: 512,
  maxStackBytes: 8 * 1024,
});

type ErrorLike = { message: string; name?: string; stack?: string };

function extractErrorLike(err: unknown): ErrorLike | null {
  if (!err || typeof err !== "object") return null;
  try {
    const rec = err as Record<string, unknown>;
    const message = rec["message"];
    if (typeof message !== "string") return null;
    const name = rec["name"];
    const stack = rec["stack"];
    return {
      message,
      ...(typeof name === "string" ? { name } : {}),
      ...(typeof stack === "string" ? { stack } : {}),
    };
  } catch {
    return null;
  }
}

function safeNonErrorMessageInput(err: unknown): string {
  if (err === null) return "null";
  switch (typeof err) {
    case "string":
      return err;
    case "number":
    case "boolean":
    case "bigint":
    case "symbol":
    case "undefined":
      return String(err);
    default:
      // Avoid calling toString() on arbitrary objects/functions.
      return "Error";
  }
}

export function serializeErrorForWorker(
  err: unknown,
  limits: ErrorByteLimits = DEFAULT_ERROR_BYTE_LIMITS,
): WorkerSerializedError {
  const like = extractErrorLike(err);
  if (like) {
    const message = formatOneLineUtf8(like.message, limits.maxMessageBytes) || "Error";
    const nameRaw = like.name;
    const name = typeof nameRaw === "string" ? formatOneLineUtf8(nameRaw, limits.maxNameBytes) || "Error" : undefined;
    const stack = typeof like.stack === "string" ? truncateUtf8(like.stack, limits.maxStackBytes) : undefined;
    return { message, ...(name ? { name } : {}), ...(stack ? { stack } : {}) };
  }

  const message = formatOneLineUtf8(safeNonErrorMessageInput(err), limits.maxMessageBytes) || "Error";
  return { message };
}

export function serializeErrorForProtocol(
  err: unknown,
  limits: ErrorByteLimits = DEFAULT_ERROR_BYTE_LIMITS,
): ProtocolSerializedError {
  const like = extractErrorLike(err);
  if (like) {
    const name = formatOneLineUtf8(like.name || "Error", limits.maxNameBytes) || "Error";
    const message = formatOneLineUtf8(like.message, limits.maxMessageBytes) || "Error";
    const stack = typeof like.stack === "string" ? truncateUtf8(like.stack, limits.maxStackBytes) : undefined;
    return { name, message, ...(stack ? { stack } : {}) };
  }

  const message = formatOneLineUtf8(safeNonErrorMessageInput(err), limits.maxMessageBytes) || "Error";
  return { name: "Error", message };
}

