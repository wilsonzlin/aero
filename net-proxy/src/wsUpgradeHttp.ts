import type { Duplex } from "node:stream";

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
  const body = `${message}\n`;
  socket.end(
    [
      `HTTP/1.1 ${status} ${httpStatusText(status)}`,
      "Content-Type: text/plain; charset=utf-8",
      `Content-Length: ${Buffer.byteLength(body)}`,
      "Connection: close",
      "",
      body
    ].join("\r\n")
  );
}

