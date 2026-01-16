/**
 * Aero WASM JIT CSP PoC
 *
 * This file is intentionally plain JS (no bundler/build step), so it can be served
 * by the tiny CSP test server (`server/poc-server.mjs`) with precise headers.
 */

import { formatOneLineError } from "../_shared/text_one_line.js";

const Op = {
  PushI32: 0x01,
  Add: 0x02,
  Mul: 0x03,
  Return: 0x04,
};

function encodeDemoProgram() {
  // Computes: (10 + 32) * 2 = 84.
  const bytes = [];
  const push = (val) => {
    bytes.push(Op.PushI32);
    bytes.push(val & 0xff, (val >> 8) & 0xff, (val >> 16) & 0xff, (val >> 24) & 0xff);
  };

  push(10);
  push(32);
  bytes.push(Op.Add);
  push(2);
  bytes.push(Op.Mul);
  bytes.push(Op.Return);

  return new Uint8Array(bytes);
}

function interpretBytecode(program) {
  const stack = [];
  let ip = 0;

  const readI32 = () => {
    if (ip + 4 > program.length) throw new Error('bytecode: truncated i32 immediate');
    const val =
      (program[ip] | (program[ip + 1] << 8) | (program[ip + 2] << 16) | (program[ip + 3] << 24)) | 0;
    ip += 4;
    return val;
  };

  while (ip < program.length) {
    const op = program[ip++];
    switch (op) {
      case Op.PushI32:
        stack.push(readI32());
        break;
      case Op.Add: {
        const a = stack.pop();
        const b = stack.pop();
        if (a === undefined || b === undefined) throw new Error('bytecode: stack underflow on ADD');
        stack.push((b + a) | 0);
        break;
      }
      case Op.Mul: {
        const a = stack.pop();
        const b = stack.pop();
        if (a === undefined || b === undefined) throw new Error('bytecode: stack underflow on MUL');
        stack.push(Math.imul(b, a));
        break;
      }
      case Op.Return: {
        const res = stack.pop();
        if (res === undefined) throw new Error('bytecode: stack underflow on RETURN');
        return res | 0;
      }
      default:
        throw new Error(`bytecode: unknown opcode 0x${op.toString(16)}`);
    }
  }

  throw new Error('bytecode: program terminated without RETURN');
}

function encodeU32Leb(value) {
  const bytes = [];
  let v = value >>> 0;
  do {
    let b = v & 0x7f;
    v >>>= 7;
    if (v !== 0) b |= 0x80;
    bytes.push(b);
  } while (v !== 0);
  return bytes;
}

function encodeI32Leb(value) {
  const bytes = [];
  let v = value | 0;
  let more = true;
  while (more) {
    let b = v & 0x7f;
    v >>= 7;
    const signBit = (b & 0x40) !== 0;
    more = !((v === 0 && !signBit) || (v === -1 && signBit));
    if (more) b |= 0x80;
    bytes.push(b);
  }
  return bytes;
}

function encodeString(str) {
  const utf8 = textEncoder.encode(str);
  return [...encodeU32Leb(utf8.length), ...utf8];
}

function section(id, payload) {
  return [id, ...encodeU32Leb(payload.length), ...payload];
}

function buildRunModuleI32(instructions, exportName = 'run') {
  // Module header.
  const bytes = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

  // Type section: one func type [] -> [i32].
  bytes.push(
    ...section(1, [
      ...encodeU32Leb(1),
      0x60, // func
      0x00, // param count
      0x01, // result count
      0x7f, // i32
    ]),
  );

  // Function section: one function with type index 0.
  bytes.push(...section(3, [...encodeU32Leb(1), 0x00]));

  // Export section: export func 0.
  bytes.push(...section(7, [...encodeU32Leb(1), ...encodeString(exportName), 0x00, 0x00]));

  // Code section: one body.
  const body = [
    0x00, // local decl count
    ...instructions,
    0x0b, // end
  ];
  bytes.push(...section(10, [...encodeU32Leb(1), ...encodeU32Leb(body.length), ...body]));

  return new Uint8Array(bytes);
}

function compileBytecodeToWasm(program) {
  const instructions = [];
  let ip = 0;

  const readI32 = () => {
    if (ip + 4 > program.length) throw new Error('bytecode: truncated i32 immediate');
    const val =
      (program[ip] | (program[ip + 1] << 8) | (program[ip + 2] << 16) | (program[ip + 3] << 24)) | 0;
    ip += 4;
    return val;
  };

  while (ip < program.length) {
    const op = program[ip++];
    switch (op) {
      case Op.PushI32:
        instructions.push(0x41, ...encodeI32Leb(readI32()));
        break;
      case Op.Add:
        instructions.push(0x6a);
        break;
      case Op.Mul:
        instructions.push(0x6c);
        break;
      case Op.Return:
        ip = program.length;
        break;
      default:
        throw new Error(`bytecode: unknown opcode 0x${op.toString(16)}`);
    }
  }

  return buildRunModuleI32(instructions, 'run');
}

async function runDynamicWasm(program, method) {
  const wasmBytes = compileBytecodeToWasm(program);
  if (method === 'WebAssembly.compile') {
    const mod = await WebAssembly.compile(wasmBytes);
    const inst = await WebAssembly.instantiate(mod);
    return inst.exports.run() | 0;
  }
  const mod = new WebAssembly.Module(wasmBytes);
  const inst = new WebAssembly.Instance(mod);
  return inst.exports.run() | 0;
}

let megaModulePromise = null;
async function runMegaModule(program) {
  if (!megaModulePromise) {
    const url = '/wasm-jit-csp/mega-module.wasm';
    if (WebAssembly.instantiateStreaming) {
      megaModulePromise = WebAssembly.instantiateStreaming(fetch(url), {}).then((r) => r.instance.exports);
    } else {
      megaModulePromise = fetch(url)
        .then((r) => r.arrayBuffer())
        .then((bytes) => WebAssembly.instantiate(bytes, {}))
        .then((r) => r.instance.exports);
    }
  }
  const exp = await megaModulePromise;
  const mem = new Uint8Array(exp.memory.buffer);
  mem.set(program, 0);
  return exp.run(0, program.length) | 0;
}

async function detectCapabilities() {
  const demo = encodeDemoProgram();
  const wasmBytes = compileBytecodeToWasm(demo);

  let webassembly_compile = false;
  let webassembly_module = false;

  try {
    const mod = await WebAssembly.compile(wasmBytes);
    const inst = await WebAssembly.instantiate(mod);
    webassembly_compile = typeof inst.exports.run === 'function' && Number.isInteger(inst.exports.run());
  } catch {
    webassembly_compile = false;
  }

  try {
    const mod = new WebAssembly.Module(wasmBytes);
    const inst = new WebAssembly.Instance(mod);
    webassembly_module = typeof inst.exports.run === 'function' && Number.isInteger(inst.exports.run());
  } catch {
    webassembly_module = false;
  }

  return {
    cross_origin_isolated: globalThis.crossOriginIsolated === true,
    shared_array_buffer: typeof SharedArrayBuffer !== 'undefined',
    jit_dynamic_wasm: webassembly_compile && webassembly_module,
    dynamic_wasm_compile: {
      webassembly_compile,
      webassembly_module,
    },
  };
}

function getBenchIterations() {
  const iters = Number.parseInt(new URLSearchParams(location.search).get('bench') ?? '25', 10);
  if (!Number.isFinite(iters) || iters < 0) return 25;
  return Math.min(iters, 200);
}

function getRequestedTier() {
  const tier = new URLSearchParams(location.search).get('tier');
  if (tier === 'dynamic-wasm' || tier === 'mega-module' || tier === 'js-interpreter') return tier;
  return null;
}

async function measureUserAgentSpecificMemory() {
  if (typeof performance.measureUserAgentSpecificMemory !== 'function') return null;
  try {
    const result = await performance.measureUserAgentSpecificMemory();
    return typeof result?.bytes === 'number' ? result.bytes : null;
  } catch {
    return null;
  }
}

async function benchDynamicCompile(iters, method) {
  const demo = encodeDemoProgram();
  const t0 = performance.now();
  for (let i = 0; i < iters; i++) {
    await runDynamicWasm(demo, method);
  }
  const t1 = performance.now();
  return (t1 - t0) / Math.max(iters, 1);
}

function formatMs(n) {
  if (!Number.isFinite(n)) return 'n/a';
  return `${n.toFixed(2)}ms`;
}

async function main() {
  const modeEl = document.getElementById('mode');
  const reportEl = document.getElementById('report');
  if (!reportEl) throw new Error('missing #report element');
  if (modeEl) modeEl.textContent = `Path: ${location.pathname}`;

  const iters = getBenchIterations();
  const requested_tier = getRequestedTier();
  const capabilities = await detectCapabilities();
  const demo = encodeDemoProgram();

  let selected_tier = 'js-interpreter';
  let result = interpretBytecode(demo);

  const tryMegaModule = async () => {
    try {
      selected_tier = 'mega-module';
      result = await runMegaModule(demo);
    } catch {
      selected_tier = 'js-interpreter';
      result = interpretBytecode(demo);
    }
  };

  if (requested_tier === 'js-interpreter') {
    selected_tier = 'js-interpreter';
    result = interpretBytecode(demo);
  } else if (requested_tier === 'mega-module') {
    await tryMegaModule();
  } else {
    if (capabilities.jit_dynamic_wasm) {
      selected_tier = 'dynamic-wasm';
      result = await runDynamicWasm(demo, 'WebAssembly.compile');
    } else {
      await tryMegaModule();
    }
  }

  const uaMemoryBefore = await measureUserAgentSpecificMemory();
  const jsHeapBefore = performance.memory?.usedJSHeapSize ?? null;

  let benchCompileAvgMs = null;
  let benchModuleAvgMs = null;
  if (capabilities.jit_dynamic_wasm && iters > 0) {
    benchCompileAvgMs = await benchDynamicCompile(iters, 'WebAssembly.compile');
    benchModuleAvgMs = await benchDynamicCompile(iters, 'new WebAssembly.Module');
  }

  const jsHeapAfter = performance.memory?.usedJSHeapSize ?? null;
  const uaMemoryAfter = await measureUserAgentSpecificMemory();

  const state = {
    ready: true,
    capabilities,
    execution: { requested_tier, selected_tier, result },
    benchmarks: {
      iterations: iters,
      dynamic_compile_avg_ms: benchCompileAvgMs,
      dynamic_module_avg_ms: benchModuleAvgMs,
      js_heap_used_before: jsHeapBefore,
      js_heap_used_after: jsHeapAfter,
      ua_memory_bytes_before: uaMemoryBefore,
      ua_memory_bytes_after: uaMemoryAfter,
    },
  };

  globalThis.__aeroWasmJitCspPoc = state;

  reportEl.textContent = [
    `crossOriginIsolated: ${String(capabilities.cross_origin_isolated)}`,
    `SharedArrayBuffer: ${String(capabilities.shared_array_buffer)}`,
    `jit_dynamic_wasm: ${String(capabilities.jit_dynamic_wasm)}`,
    ``,
    `requested tier: ${requested_tier ?? '(auto)'}`,
    `selected tier: ${selected_tier}`,
    `demo result: ${result}`,
    ``,
    `bench iters: ${iters}`,
    `dynamic compile avg: ${benchCompileAvgMs === null ? 'n/a' : formatMs(benchCompileAvgMs)}`,
    `dynamic module avg: ${benchModuleAvgMs === null ? 'n/a' : formatMs(benchModuleAvgMs)}`,
    ``,
    `ua memory before: ${uaMemoryBefore === null ? 'n/a' : String(uaMemoryBefore)}`,
    `ua memory after:  ${uaMemoryAfter === null ? 'n/a' : String(uaMemoryAfter)}`,
    ``,
    `js heap before: ${jsHeapBefore === null ? 'n/a' : String(jsHeapBefore)}`,
    `js heap after:  ${jsHeapAfter === null ? 'n/a' : String(jsHeapAfter)}`,
  ].join('\n');
}

main().catch((err) => {
  const reportEl = document.getElementById('report');
  const message = formatOneLineError(err, 512, { includeNameFallback: true });
  if (reportEl) reportEl.textContent = `Error: ${message}`;
  globalThis.__aeroWasmJitCspPoc = { ready: true, error: message };
});

