import type { Duplex } from "node:stream";

import { formatOneLineUtf8 } from "./text";

import { encodeHttpTextResponse } from "./httpTextResponse";
import { endThenDestroyQuietly } from "./socketEndThenDestroy";

const MAX_UPGRADE_ERROR_MESSAGE_BYTES = 512;

function httpStatusText(status: number): string {
  switch (status) {
    case 500:
      return "Internal Server Error";
    case 400:
      return "Bad Request";
    case 401:
      return "Unauthorized";
    case 403:
      return "Forbidden";
    case 404:
      return "Not Found";
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

export function rejectWsUpgrade(socket: Duplex, status: number, message: string): void {
  const statusText = httpStatusText(status);
  const safeMessage = formatOneLineUtf8(message, MAX_UPGRADE_ERROR_MESSAGE_BYTES) || statusText;
  const res = encodeHttpTextResponse({ statusCode: status, statusText, bodyText: `${safeMessage}\n` });
  endThenDestroyQuietly(socket, res);
}

