// Minimal module worker used by Playwright to validate:
// - `worker-src` allows same-origin module workers
// - COOP/COEP/CORP headers are set on worker script responses
// - CSP allows WASM compilation (`'wasm-unsafe-eval'`) while blocking `eval()`

const UTF8 = Object.freeze({ encoding: "utf-8" });
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder(UTF8.encoding);

function coerceString(input) {
  try {
    return String(input ?? "");
  } catch {
    return "";
  }
}

function formatOneLineUtf8(input, maxBytes) {
  if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
  if (maxBytes === 0) return "";

  const buf = new Uint8Array(maxBytes);
  let written = 0;
  let pendingSpace = false;
  for (const ch of coerceString(input)) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) {
      pendingSpace = written > 0;
      continue;
    }

    if (pendingSpace) {
      const spaceRes = textEncoder.encodeInto(" ", buf.subarray(written));
      if (spaceRes.written === 0) break;
      written += spaceRes.written;
      pendingSpace = false;
      if (written >= maxBytes) break;
    }

    const res = textEncoder.encodeInto(ch, buf.subarray(written));
    if (res.written === 0) break;
    written += res.written;
    if (written >= maxBytes) break;
  }
  return written === 0 ? "" : textDecoder.decode(buf.subarray(0, written));
}

function safeErrorMessageInput(err) {
  if (err === null) return "null";
  const t = typeof err;
  if (t === "string") return err;
  if (t === "number" || t === "boolean" || t === "bigint" || t === "symbol" || t === "undefined") return String(err);
  if (t === "object") {
    try {
      const msg = err && typeof err.message === "string" ? err.message : "";
      if (msg) return msg;
    } catch {
      // ignore
    }
  }
  return "Error";
}

function formatOneLineError(err, maxBytes) {
  return formatOneLineUtf8(safeErrorMessageInput(err), maxBytes) || "Error";
}

async function run() {
  try {
    // Validate that `script-src 'wasm-unsafe-eval'` is working.
    const wasmBytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    await WebAssembly.compile(wasmBytes);
  } catch (error) {
    const msg = formatOneLineError(error, 256);
    self.postMessage(`wasm_compile_failed:${msg}`);
    return;
  }

  try {
    // eslint-disable-next-line no-eval
    eval('1+1');
    self.postMessage('eval_allowed');
  } catch {
    self.postMessage('ok');
  }
}

run();
