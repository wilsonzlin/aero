/**
 * Best-effort classification of "expected" abort/disconnect errors for HTTP streaming.
 *
 * Node stream pipelines often surface these when the client disconnects mid-response.
 * These should not be treated as actionable server-side failures.
 *
 * @param {unknown} err
 * @returns {boolean}
 */
export function isExpectedStreamAbort(err) {
  if (!err || typeof err !== "object") return false;
  let code;
  try {
    code = err.code;
  } catch {
    code = undefined;
  }

  if (code === undefined) {
    let cause;
    try {
      cause = err.cause;
    } catch {
      cause = undefined;
    }
    if (cause && typeof cause === "object") {
      try {
        code = cause.code;
      } catch {
        code = undefined;
      }
    }
  }
  return code === "ERR_STREAM_PREMATURE_CLOSE" || code === "ECONNRESET" || code === "EPIPE";
}
