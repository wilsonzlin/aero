import type { Duplex } from "node:stream";

import { formatOneLineUtf8 } from "./text";

const MAX_UPGRADE_ERROR_MESSAGE_BYTES = 512;

function httpStatusText(status: number): string {
  switch (status) {
    case 400:
      return "Bad Request";
    case 404:
      return "Not Found";
    case 414:
      return "URI Too Long";
    default:
      return "Error";
  }
}

export function rejectWsUpgrade(socket: Duplex, status: number, message: string): void {
  const safeMessage = formatOneLineUtf8(message, MAX_UPGRADE_ERROR_MESSAGE_BYTES) || httpStatusText(status);
  const body = `${safeMessage}\n`;
  const response = [
    `HTTP/1.1 ${status} ${httpStatusText(status)}`,
    "Content-Type: text/plain; charset=utf-8",
    `Content-Length: ${Buffer.byteLength(body)}`,
    "Connection: close",
    "",
    body,
  ].join("\r\n");
  try {
    socket.end(response);
  } catch {
    try {
      socket.destroy();
    } catch {
      // ignore
    }
  }
}

