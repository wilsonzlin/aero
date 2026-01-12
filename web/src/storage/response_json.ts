function parsePositiveIntHeader(value: string | null): number | null {
  if (!value) return null;
  const trimmed = value.trim();
  if (!trimmed) return null;
  // `Content-Length` is defined as a decimal integer.
  if (!/^\d+$/.test(trimmed)) return null;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || !Number.isSafeInteger(n) || n < 0) return null;
  return n;
}

async function cancelBody(resp: Response): Promise<void> {
  try {
    await resp.body?.cancel();
  } catch {
    // ignore best-effort cancellation failures
  }
}

export async function readResponseBytesWithLimit(
  resp: Response,
  opts: { maxBytes: number; label: string },
): Promise<Uint8Array<ArrayBuffer>> {
  const maxBytes = opts.maxBytes;
  if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0) {
    throw new Error(`readResponseBytesWithLimit: maxBytes must be a positive safe integer (got ${maxBytes})`);
  }

  const contentLength = parsePositiveIntHeader(resp.headers.get("content-length"));
  if (contentLength !== null && contentLength > maxBytes) {
    await cancelBody(resp);
    throw new Error(`${opts.label} too large: max=${maxBytes} content-length=${contentLength}`);
  }

  // If `body` is missing, fall back to `arrayBuffer()` (should be rare for JSON responses).
  if (!resp.body) {
    const buf = await resp.arrayBuffer();
    if (buf.byteLength > maxBytes) {
      throw new Error(`${opts.label} too large: max=${maxBytes} actual=${buf.byteLength}`);
    }
    return new Uint8Array(buf);
  }

  const reader = resp.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      if (!value) continue;
      total += value.byteLength;
      if (total > maxBytes) {
        try {
          await reader.cancel();
        } catch {
          // ignore
        }
        throw new Error(`${opts.label} too large: max=${maxBytes}`);
      }
      chunks.push(value);
    }
  } finally {
    try {
      reader.releaseLock();
    } catch {
      // ignore
    }
  }

  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out as Uint8Array<ArrayBuffer>;
}

export async function readJsonResponseWithLimit(resp: Response, opts: { maxBytes: number; label: string }): Promise<unknown> {
  const bytes = await readResponseBytesWithLimit(resp, opts);
  const text = new TextDecoder().decode(bytes);
  try {
    return JSON.parse(text) as unknown;
  } catch (err) {
    // Include the label to make debugging easier.
    throw new Error(`${opts.label} is not valid JSON (${err instanceof Error ? err.message : String(err)})`);
  }
}

