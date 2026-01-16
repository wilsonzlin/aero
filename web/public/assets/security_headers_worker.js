// Minimal module worker used by Playwright to validate:
// - `worker-src` allows same-origin module workers
// - COOP/COEP/CORP headers are set on worker script responses
// - CSP allows WASM compilation (`'wasm-unsafe-eval'`) while blocking `eval()`

import { formatOneLineError } from "../_shared/text_one_line.js";

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
