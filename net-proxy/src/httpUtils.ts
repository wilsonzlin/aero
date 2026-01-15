import type http from "node:http";

export async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  let handle: NodeJS.Timeout | null = null;
  const timeout = new Promise<never>((_resolve, reject) => {
    handle = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs}ms`)), timeoutMs);
    handle.unref();
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (handle) clearTimeout(handle);
  }
}

export async function readRequestBodyWithLimit(
  req: http.IncomingMessage,
  maxBytes: number
): Promise<{ body: Buffer; tooLarge: boolean }> {
  const chunks: Buffer[] = [];
  let storedBytes = 0;
  let totalBytes = 0;

  return await new Promise((resolve, reject) => {
    req.on("error", reject);
    req.on("data", (chunk: Buffer) => {
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      totalBytes += buf.length;
      const remaining = maxBytes - storedBytes;
      if (remaining <= 0) return;
      const slice = buf.length > remaining ? buf.subarray(0, remaining) : buf;
      chunks.push(slice);
      storedBytes += slice.length;
    });
    req.on("end", () => {
      resolve({ body: Buffer.concat(chunks, storedBytes), tooLarge: totalBytes > maxBytes });
    });
  });
}

