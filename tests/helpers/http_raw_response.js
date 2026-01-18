import net from "node:net";

import { unrefBestEffort } from "../../src/unref_safe.js";

function parseHttpResponseHead(headText) {
  const lines = headText.split("\r\n");
  const statusLine = lines[0] ?? "";
  /** @type {Record<string, string>} */
  const headers = {};

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    if (!line) continue;
    const idx = line.indexOf(":");
    if (idx === -1) continue;

    const key = line.slice(0, idx).trim().toLowerCase();
    if (!key) continue;
    headers[key] = line.slice(idx + 1).trim();
  }

  return { statusLine, headers };
}

export async function sendRawHttpRequest(host, port, request, opts = {}) {
  const timeoutMs = Number.isFinite(opts.timeoutMs) ? opts.timeoutMs : 2000;
  const maxBytes = Number.isFinite(opts.maxBytes) ? opts.maxBytes : 64 * 1024;

  return await new Promise((resolve, reject) => {
    const socket = net.connect({ host, port });
    let buf = Buffer.alloc(0);
    let done = false;

    const cleanup = () => {
      if (done) return;
      done = true;
      socket.removeAllListeners();
      try {
        socket.destroy();
      } catch {
        // ignore
      }
    };

    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error("timeout: sendRawHttpRequest"));
    }, timeoutMs);
    unrefBestEffort(timeout);

    socket.on("error", (err) => {
      clearTimeout(timeout);
      cleanup();
      reject(err);
    });

    socket.on("data", (chunk) => {
      buf = buf.length === 0 ? chunk : Buffer.concat([buf, chunk]);
      if (buf.length > maxBytes) {
        clearTimeout(timeout);
        cleanup();
        reject(new Error("sendRawHttpRequest exceeded maxBytes"));
        return;
      }

      const headerEnd = buf.indexOf("\r\n\r\n");
      if (headerEnd === -1) return;

      const headText = buf.subarray(0, headerEnd).toString("utf8");
      const { statusLine, headers } = parseHttpResponseHead(headText);
      const rawLen = headers["content-length"];
      const len = rawLen === undefined ? 0 : Number.parseInt(rawLen, 10);
      if (!Number.isFinite(len) || len < 0) {
        clearTimeout(timeout);
        cleanup();
        reject(new Error("sendRawHttpRequest invalid Content-Length"));
        return;
      }
      if (len > maxBytes) {
        clearTimeout(timeout);
        cleanup();
        reject(new Error("sendRawHttpRequest Content-Length exceeds maxBytes"));
        return;
      }

      const bodyStart = headerEnd + 4;
      if (buf.length < bodyStart + len) return;

      const body = buf.subarray(bodyStart, bodyStart + len);
      clearTimeout(timeout);
      cleanup();
      resolve({ statusLine, headers, body });
    });

    try {
      socket.write(request);
    } catch (err) {
      clearTimeout(timeout);
      cleanup();
      reject(err);
    }
  });
}
