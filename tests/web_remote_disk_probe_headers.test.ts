import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { once } from "node:events";

import { probeRemoteDisk } from "../web/src/platform/remote_disk";

import { sendEmpty, sendText } from "./helpers/http_test_response.js";

function startRangeServer(opts: {
  contentRange?: string;
  cacheControl?: string;
  contentEncoding?: string;
  includeContentLengthOnHead?: boolean;
}): Promise<{ baseUrl: string; close: () => Promise<void> }> {
  const totalSize = 1024;

  const server = http.createServer((req, res) => {
    const rawUrl = req.url ?? "/";
    let url: URL;
    try {
      url = new URL(rawUrl, "http://localhost");
    } catch {
      sendText(res, 400, "bad request");
      return;
    }
    if (url.pathname !== "/disk") {
      sendText(res, 404, "not found");
      return;
    }

    const method = req.method ?? "GET";
    const allow = "GET, HEAD, OPTIONS";
    if (method === "OPTIONS") {
      sendEmpty(res, 204, { allow });
      return;
    }
    if (method !== "GET" && method !== "HEAD") {
      sendEmpty(res, 405, { allow });
      return;
    }

    if (req.method === "HEAD") {
      res.statusCode = 200;
      if (opts.includeContentLengthOnHead !== false) {
        res.setHeader("Content-Length", String(totalSize));
      }
      res.end();
      return;
    }

    const range = req.headers.range;
    if (req.method === "GET" && range === "bytes=0-0") {
      res.statusCode = 206;
      res.setHeader("Accept-Ranges", "bytes");
      res.setHeader("Content-Length", "1");
      res.setHeader("Content-Range", opts.contentRange ?? "bytes 0-0/1024");
      res.setHeader("Cache-Control", opts.cacheControl ?? "no-transform");
      if (opts.contentEncoding) res.setHeader("Content-Encoding", opts.contentEncoding);
      res.end(Buffer.from([0x00]));
      return;
    }

    sendEmpty(res, 416);
  });

  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", async () => {
      try {
        const address = server.address();
        if (!address || typeof address === "string") throw new Error("Unexpected server address");
        const baseUrl = `http://127.0.0.1:${address.port}`;
        resolve({
          baseUrl,
          close: async () =>
            await new Promise<void>((resolveClose, rejectClose) => {
              server.close((err) => (err ? rejectClose(err) : resolveClose()));
            }),
        });
      } catch (err) {
        server.close(() => reject(err));
      }
    });
  });
}

test("probeRemoteDisk: accepts identity Content-Encoding and Cache-Control: no-transform", async () => {
  const server = await startRangeServer({ contentEncoding: "identity" });
  try {
    const result = await probeRemoteDisk(`${server.baseUrl}/disk`);
    assert.equal(result.partialOk, true);
    assert.equal(result.size, 1024);
    assert.equal(result.rangeProbeStatus, 206);
  } finally {
    await server.close();
  }
});

test("probeRemoteDisk: rejects oversized Cache-Control values (defense-in-depth)", async () => {
  const huge = `no-transform, ${"a".repeat(10_000)}`;
  const server = await startRangeServer({ cacheControl: huge });
  try {
    await assert.rejects(async () => {
      await probeRemoteDisk(`${server.baseUrl}/disk`);
    });
  } finally {
    await server.close();
  }
});

test("probeRemoteDisk: rejects oversized Content-Encoding values (defense-in-depth)", async () => {
  const huge = `identity ${"a".repeat(10_000)}`;
  const server = await startRangeServer({ contentEncoding: huge });
  try {
    await assert.rejects(async () => {
      await probeRemoteDisk(`${server.baseUrl}/disk`);
    });
  } finally {
    await server.close();
  }
});

test("probeRemoteDisk: rejects oversized Content-Range before parsing", async () => {
  const huge = `bytes 0-0/1024;${"a".repeat(10_000)}`;
  const server = await startRangeServer({
    includeContentLengthOnHead: false,
    contentRange: huge,
  });
  try {
    await assert.rejects(async () => {
      await probeRemoteDisk(`${server.baseUrl}/disk`);
    });
  } finally {
    await server.close();
  }
});

