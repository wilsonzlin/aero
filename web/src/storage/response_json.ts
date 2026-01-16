import { formatOneLineError } from "../text";

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

export class ResponseTooLargeError extends Error {
  override name = "ResponseTooLargeError";

  readonly maxBytes: number;
  readonly contentLength: number | null;
  readonly actualBytes: number | null;

  constructor(opts: { label: string; maxBytes: number; contentLength?: number | null; actualBytes?: number | null }) {
    const parts: string[] = [`max=${opts.maxBytes}`];
    if (opts.contentLength !== undefined && opts.contentLength !== null) {
      parts.push(`content-length=${opts.contentLength}`);
    }
    if (opts.actualBytes !== undefined && opts.actualBytes !== null) {
      parts.push(`actual=${opts.actualBytes}`);
    }
    super(`${opts.label} too large: ${parts.join(" ")}`);
    this.maxBytes = opts.maxBytes;
    this.contentLength = opts.contentLength ?? null;
    this.actualBytes = opts.actualBytes ?? null;
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
    throw new ResponseTooLargeError({ label: opts.label, maxBytes, contentLength });
  }

  // If `body` is missing, fall back to `arrayBuffer()` (should be rare for JSON responses).
  if (!resp.body) {
    const buf = await resp.arrayBuffer();
    if (buf.byteLength > maxBytes) {
      throw new ResponseTooLargeError({ label: opts.label, maxBytes, actualBytes: buf.byteLength });
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
        throw new ResponseTooLargeError({ label: opts.label, maxBytes });
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
    throw new Error(`${opts.label} is not valid JSON (${formatOneLineError(err, 256)})`);
  }
}

export async function readTextResponseWithLimit(resp: Response, opts: { maxBytes: number; label: string }): Promise<string> {
  const bytes = await readResponseBytesWithLimit(resp, opts);
  return new TextDecoder().decode(bytes);
}
