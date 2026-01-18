import { formatOneLineUtf8 } from "./text.js";
import { encodeHttpTextResponse } from "./http_text_response.js";
import { endThenDestroyQuietly } from "./socket_end_then_destroy.js";

const MAX_UPGRADE_ERROR_MESSAGE_BYTES = 512;

function httpStatusText(status) {
  switch (status) {
    case 400:
      return "Bad Request";
    case 401:
      return "Unauthorized";
    case 403:
      return "Forbidden";
    case 404:
      return "Not Found";
    case 500:
      return "Internal Server Error";
    case 414:
      return "URI Too Long";
    case 429:
      return "Too Many Requests";
    case 502:
      return "Bad Gateway";
    case 503:
      return "Service Unavailable";
    default:
      return "Error";
  }
}

/**
 * Best-effort HTTP response for failed upgrade requests (e.g. WebSocket handshake rejection).
 * Intended for Node `http.Server` "upgrade" handlers.
 *
 * @param {import("node:stream").Duplex} socket
 * @param {number} statusCode
 * @param {unknown} message
 */
export function rejectHttpUpgrade(socket, statusCode, message) {
  try {
    const statusText = httpStatusText(statusCode);
    const safeMessage = formatOneLineUtf8(message, MAX_UPGRADE_ERROR_MESSAGE_BYTES) || statusText;
    endThenDestroyQuietly(
      socket,
      encodeHttpTextResponse({
        statusCode,
        statusText,
        bodyText: `${safeMessage}\n`,
      }),
    );
  } catch {
    try {
      socket?.destroy?.();
    } catch {
      // ignore
    }
  }
}
