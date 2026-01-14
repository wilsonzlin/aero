import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import type { Socket } from "node:net";
import { afterEach, describe, expect, it } from "vitest";

import { probeRemoteDisk } from "./remote_disk";

async function withServer(
  handler: (req: IncomingMessage, res: ServerResponse) => void,
): Promise<{ baseUrl: string; close: () => Promise<void> }> {
  const sockets = new Set<Socket>();
  const server = createServer(handler);
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as AddressInfo).port;

  return {
    baseUrl: `http://127.0.0.1:${port}`,
    close: async () => {
      for (const s of sockets) {
        try {
          s.destroy();
        } catch {
          // ignore
        }
      }
      await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
    },
  };
}

let closeServer: (() => Promise<void>) | null = null;

afterEach(async () => {
  if (closeServer) await closeServer();
  closeServer = null;
});

describe("probeRemoteDisk", () => {
  it("cancels the probe body when the server ignores Range and returns 200", async () => {
    // This response is intentionally large and streamed in chunks. If probeRemoteDisk doesn't
    // cancel the body, the server would finish sending the entire payload.
    const totalToSend = 1024 * 1024; // 1 MiB
    const chunk = Buffer.alloc(64 * 1024);

    let firstEvent: "close" | "finish" | null = null;
    let bytesSent = 0;
    let probeResponseDone: Promise<void> | null = null;

    const { baseUrl, close } = await withServer((req, res) => {
      if ((req.url ?? "") !== "/image.bin") {
        res.statusCode = 404;
        res.end("not found");
        return;
      }

      if (req.method === "HEAD") {
        res.statusCode = 200;
        res.setHeader("accept-ranges", "bytes");
        res.setHeader("content-length", "1024");
        res.end();
        return;
      }

      if (req.method === "GET") {
        // Ignore the Range header and return a full representation.
        res.statusCode = 200;
        res.setHeader("accept-ranges", "bytes");
        res.setHeader("content-length", String(totalToSend));

        probeResponseDone = new Promise<void>((resolve) => {
          const onClose = () => {
            if (!firstEvent) firstEvent = "close";
            resolve();
          };
          const onFinish = () => {
            if (!firstEvent) firstEvent = "finish";
            resolve();
          };
          res.once("close", onClose);
          res.once("finish", onFinish);
        });

        // Stream the body slowly enough that cancellation matters.
        let timer: ReturnType<typeof setInterval> | null = null;
        let sent = 0;
        const writeChunk = () => {
          if (sent >= totalToSend) {
            if (timer) clearInterval(timer);
            res.end();
            return;
          }
          const remaining = totalToSend - sent;
          const slice = remaining >= chunk.length ? chunk : chunk.subarray(0, remaining);
          try {
            res.write(slice);
          } catch {
            // Connection may have been closed by the client.
            if (timer) clearInterval(timer);
            return;
          }
          sent += slice.length;
          bytesSent = sent;
        };

        // Write once immediately, then continue streaming.
        writeChunk();
        timer = setInterval(writeChunk, 10);
        return;
      }

      res.statusCode = 405;
      res.end("method not allowed");
    });
    closeServer = close;

    const result = await probeRemoteDisk(`${baseUrl}/image.bin`, { credentials: "omit" });
    expect(result.partialOk).toBe(false);
    expect(result.rangeProbeStatus).toBe(200);

    // Ensure we observed the client cancel/close the stream before the full body was sent.
    await Promise.race([
      probeResponseDone,
      new Promise<void>((_, reject) => setTimeout(() => reject(new Error("timed out waiting for probe response")), 1000)),
    ]);

    expect(firstEvent).toBe("close");
    expect(bytesSent).toBeLessThan(totalToSend);
  });

  it("rejects Range probes with non-identity Content-Encoding", async () => {
    const { baseUrl, close } = await withServer((req, res) => {
      if ((req.url ?? "") !== "/image.bin") {
        res.statusCode = 404;
        res.end("not found");
        return;
      }

      if (req.method === "HEAD") {
        res.statusCode = 200;
        res.setHeader("accept-ranges", "bytes");
        res.setHeader("content-length", "1024");
        res.end();
        return;
      }

      if (req.method === "GET") {
        const range = req.headers.range;
        if (typeof range !== "string") {
          res.statusCode = 400;
          res.end("missing Range");
          return;
        }
        if (range !== "bytes=0-0") {
          res.statusCode = 416;
          res.end();
          return;
        }

        res.statusCode = 206;
        res.setHeader("accept-ranges", "bytes");
        res.setHeader("cache-control", "no-transform");
        res.setHeader("content-range", "bytes 0-0/1024");
        res.setHeader("content-length", "1");
        // Disk streaming requires identity/absent encoding.
        res.setHeader("content-encoding", "gzip");
        res.end(Buffer.from([0]));
        return;
      }

      res.statusCode = 405;
      res.end("method not allowed");
    });
    closeServer = close;

    await expect(probeRemoteDisk(`${baseUrl}/image.bin`, { credentials: "omit" })).rejects.toThrow(
      /Content-Encoding/i,
    );
  });

  it("falls back to Content-Range when HEAD Content-Length is not a safe integer", async () => {
    const { baseUrl, close } = await withServer((req, res) => {
      if ((req.url ?? "") !== "/image.bin") {
        res.statusCode = 404;
        res.end("not found");
        return;
      }

      if (req.method === "HEAD") {
        res.statusCode = 200;
        // Not a safe JS integer (2^53).
        res.setHeader("content-length", "9007199254740992");
        res.end();
        return;
      }

      if (req.method === "GET") {
        const range = req.headers.range;
        if (typeof range !== "string" || range !== "bytes=0-0") {
          res.statusCode = 416;
          res.end();
          return;
        }
        res.statusCode = 206;
        res.setHeader("accept-ranges", "bytes");
        res.setHeader("cache-control", "no-transform");
        res.setHeader("content-range", "bytes 0-0/1024");
        res.setHeader("content-length", "1");
        res.end(Buffer.from([0]));
        return;
      }

      res.statusCode = 405;
      res.end("method not allowed");
    });
    closeServer = close;

    const result = await probeRemoteDisk(`${baseUrl}/image.bin`, { credentials: "omit" });
    expect(result.partialOk).toBe(true);
    expect(result.size).toBe(1024);
  });

  it("rejects Range probes without Cache-Control: no-transform", async () => {
    const { baseUrl, close } = await withServer((req, res) => {
      if ((req.url ?? "") !== "/image.bin") {
        res.statusCode = 404;
        res.end("not found");
        return;
      }

      if (req.method === "HEAD") {
        res.statusCode = 200;
        res.setHeader("accept-ranges", "bytes");
        res.setHeader("content-length", "1024");
        res.end();
        return;
      }

      if (req.method === "GET") {
        const range = req.headers.range;
        if (typeof range !== "string" || range !== "bytes=0-0") {
          res.statusCode = 416;
          res.end();
          return;
        }

        res.statusCode = 206;
        res.setHeader("accept-ranges", "bytes");
        res.setHeader("content-range", "bytes 0-0/1024");
        res.setHeader("content-length", "1");
        // Intentionally omit no-transform.
        res.setHeader("cache-control", "public");
        res.end(Buffer.from([0]));
        return;
      }

      res.statusCode = 405;
      res.end("method not allowed");
    });
    closeServer = close;

    await expect(probeRemoteDisk(`${baseUrl}/image.bin`, { credentials: "omit" })).rejects.toThrow(/no-transform/i);
  });
});
