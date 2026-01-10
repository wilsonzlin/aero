/// <reference lib="webworker" />

import type { CompileBlockResponse, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import {
  copyWasmBytes,
  CPU_RUNTIME_WASM_BYTES,
  SHARED_MEMORY_INITIAL_PAGES,
  SHARED_MEMORY_MAX_PAGES,
} from './wasm-bytes';

const ctx = self as unknown as DedicatedWorkerGlobalScope;

type CpuWorkerToMainMessage =
  | { type: 'CpuWorkerReady' }
  | {
      type: 'CpuWorkerResult';
      jit_executions: number;
      helper_executions: number;
      interp_executions: number;
      installed_table_index: number | null;
      runtime_installed_entry_rip: number | null;
      runtime_installed_table_index: number | null;
    }
  | { type: 'CpuWorkerError'; reason: string };

type CpuWorkerStartMessage = {
  type: 'CpuWorkerStart';
  iterations?: number;
  threshold?: number;
};

const MAX_JIT_TABLE_ENTRIES = 64;
const ENTRY_RIP = 0x1000;

type CpuRuntimeExports = {
  jit_helper: () => void;
  call_jit: (tableIndex: number) => void;
  install_jit_block: (entryRip: number, tableIndex: number) => void;
};

function postToMain(msg: CpuWorkerToMainMessage) {
  ctx.postMessage(msg);
}

type PendingCompile = {
  resolve: (msg: CompileBlockResponse) => void;
  reject: (reason: string) => void;
};

let nextCompileId = 1;
const pendingCompiles = new Map<number, PendingCompile>();

async function installJitBlock(
  resp: CompileBlockResponse,
  memory: WebAssembly.Memory,
  table: WebAssembly.Table,
  cpuRuntime: CpuRuntimeExports,
  indexToEntryRip: Map<number, number>,
  entryRipToIndex: Map<number, number>,
  evictionCursor: { value: number },
) {
  const module =
    resp.wasm_module ??
    (resp.wasm_bytes
      ? await WebAssembly.compile(resp.wasm_bytes)
      : (() => {
          throw new Error('JIT response missing both wasm_module and wasm_bytes');
        })());

  const instance = await WebAssembly.instantiate(module, {
    env: { memory },
    cpu: { jit_helper: cpuRuntime.jit_helper },
  });

  const block = (instance.exports as { block?: unknown }).block;
  if (typeof block !== 'function') {
    throw new Error('JIT block module did not export a callable `block` function');
  }

  let idx: number;
  if (table.length < MAX_JIT_TABLE_ENTRIES) {
    idx = table.grow(1);
  } else {
    // Simple bounded cache: overwrite the next slot (round-robin).
    idx = evictionCursor.value;
    evictionCursor.value = (evictionCursor.value + 1) % MAX_JIT_TABLE_ENTRIES;
    const evictedEntry = indexToEntryRip.get(idx);
    if (evictedEntry !== undefined) entryRipToIndex.delete(evictedEntry);
  }

  indexToEntryRip.set(idx, resp.entry_rip);
  entryRipToIndex.set(resp.entry_rip, idx);
  table.set(idx, block);
  cpuRuntime.install_jit_block(resp.entry_rip, idx);
}

async function runSyntheticProgram(iterations: number, threshold: number) {
  let memory: WebAssembly.Memory;
  try {
    memory = new WebAssembly.Memory({
      initial: SHARED_MEMORY_INITIAL_PAGES,
      maximum: SHARED_MEMORY_MAX_PAGES,
      shared: true,
    });
  } catch (err) {
    postToMain({
      type: 'CpuWorkerError',
      reason:
        'Failed to allocate shared WebAssembly.Memory. Is the page crossOriginIsolated?\n' +
        (err instanceof Error ? err.message : String(err)),
    });
    return;
  }

  const counters = new Int32Array(memory.buffer);
  counters[0] = 0; // JIT block executions (written by JIT block WASM).
  counters[1] = 0; // CPU helper executions (written by CPU runtime WASM).
  counters[2] = 0; // Interpreter executions (written by JS interpreter loop).
  counters[3] = -1; // Installed table index (written by CPU runtime WASM).
  counters[4] = 0; // Installed entry RIP (written by CPU runtime WASM).

  const table = new WebAssembly.Table({
    // TS libdom types still use "anyfunc"; modern browsers also accept it.
    element: 'anyfunc',
    initial: 0,
    maximum: MAX_JIT_TABLE_ENTRIES,
  });

  const cpuRuntimeModule = await WebAssembly.compile(copyWasmBytes(CPU_RUNTIME_WASM_BYTES));
  const cpuRuntimeInstance = await WebAssembly.instantiate(cpuRuntimeModule, { env: { memory, table } });
  const cpuRuntime = cpuRuntimeInstance.exports as Partial<CpuRuntimeExports>;
  if (typeof cpuRuntime.jit_helper !== 'function') throw new Error('cpu runtime missing jit_helper export');
  if (typeof cpuRuntime.call_jit !== 'function') throw new Error('cpu runtime missing call_jit export');
  if (typeof cpuRuntime.install_jit_block !== 'function') throw new Error('cpu runtime missing install_jit_block export');

  const indexToEntryRip = new Map<number, number>();
  const entryRipToIndex = new Map<number, number>();
  const evictionCursor = { value: 0 };

  const jitWorker = new Worker(new URL('./jit-worker.ts', import.meta.url), { type: 'module' });
  jitWorker.addEventListener('message', (ev: MessageEvent<JitToCpuMessage>) => {
    const msg = ev.data;
    switch (msg.type) {
      case 'CompileBlockResponse': {
        const pending = pendingCompiles.get(msg.id);
        if (!pending) return;
        pendingCompiles.delete(msg.id);
        pending.resolve(msg);
        break;
      }
      case 'CompileError': {
        const pending = pendingCompiles.get(msg.id);
        if (!pending) return;
        pendingCompiles.delete(msg.id);
        pending.reject(msg.reason);
        break;
      }
      default:
        break;
    }
  });

  const initMsg: CpuToJitMessage = { type: 'JitWorkerInit', memory };
  jitWorker.postMessage(initMsg);

  function requestCompile(entry_rip: number): Promise<CompileBlockResponse> {
    const id = nextCompileId++;
    const req: CpuToJitMessage = {
      type: 'CompileBlockRequest',
      id,
      entry_rip,
      mode: 'tier1',
      max_bytes: 0,
    };

    jitWorker.postMessage(req);

    return new Promise((resolve, reject) => {
      pendingCompiles.set(id, {
        resolve,
        reject: (reason) => reject(new Error(reason)),
      });
    });
  }

  let compilePromise: Promise<void> | null = null;

  for (let i = 0; i < iterations; i++) {
    const idx = entryRipToIndex.get(ENTRY_RIP);
    if (idx !== undefined) {
      cpuRuntime.call_jit(idx);
    } else {
      Atomics.add(counters, 2, 1);
      if (!compilePromise && counters[2] >= threshold) {
        compilePromise = requestCompile(ENTRY_RIP).then((resp) =>
          installJitBlock(
            resp,
            memory,
            table,
            cpuRuntime as CpuRuntimeExports,
            indexToEntryRip,
            entryRipToIndex,
            evictionCursor,
          ),
        );
      }
    }

    // Yield periodically so the worker stays responsive while compilation happens in parallel.
    if ((i & 0x0f) === 0) await new Promise((r) => setTimeout(r, 0));
  }

  if (compilePromise) {
    try {
      await compilePromise;
    } catch (err) {
      postToMain({
        type: 'CpuWorkerError',
        reason: `JIT compile failed: ${err instanceof Error ? err.message : String(err)}`,
      });
      return;
    }

    // Ensure we exercise the installed block at least once.
    const idx = entryRipToIndex.get(ENTRY_RIP);
    if (idx !== undefined) {
      cpuRuntime.call_jit(idx);
    }
  }

  const installedIndex = entryRipToIndex.get(ENTRY_RIP) ?? null;
  const runtimeInstalledTableIndex = counters[3] >= 0 ? counters[3] : null;
  const runtimeInstalledEntryRip = runtimeInstalledTableIndex !== null ? counters[4] : null;
  postToMain({
    type: 'CpuWorkerResult',
    jit_executions: Atomics.load(counters, 0),
    helper_executions: Atomics.load(counters, 1),
    interp_executions: Atomics.load(counters, 2),
    installed_table_index: installedIndex,
    runtime_installed_entry_rip: runtimeInstalledEntryRip,
    runtime_installed_table_index: runtimeInstalledTableIndex,
  });
}

ctx.addEventListener('message', (ev: MessageEvent<CpuWorkerStartMessage>) => {
  const msg = ev.data;
  if (msg.type !== 'CpuWorkerStart') return;
  void runSyntheticProgram(msg.iterations ?? 256, msg.threshold ?? 32);
});

postToMain({ type: 'CpuWorkerReady' });
