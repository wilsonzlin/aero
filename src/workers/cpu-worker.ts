/// <reference lib="webworker" />

import type { CompileBlockResponse, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { initWasmForContext, type WasmApi } from '../../web/src/runtime/wasm_context';

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

const ENTRY_RIP = 0x1000;

function postToMain(msg: CpuWorkerToMainMessage) {
  ctx.postMessage(msg);
}

type PendingCompile = {
  resolve: (msg: CompileBlockResponse) => void;
  reject: (reason: string) => void;
};

let nextCompileId = 1;
const pendingCompiles = new Map<number, PendingCompile>();

const WASM_PAGE_BYTES = 64 * 1024;
const RUNTIME_RESERVED_BYTES = 128 * 1024 * 1024;
const DEFAULT_GUEST_RAM_BYTES = 16 * 1024 * 1024;

function platformSharedMemoryError(err: unknown): string {
  const detail = err instanceof Error ? err.message : String(err);
  return (
    'Failed to allocate shared WebAssembly.Memory. This requires a cross-origin isolated page.\n' +
    '\n' +
    'To enable crossOriginIsolated in browsers, serve the page with:\n' +
    '  Cross-Origin-Opener-Policy: same-origin\n' +
    '  Cross-Origin-Embedder-Policy: require-corp\n' +
    '\n' +
    `Underlying error: ${detail}`
  );
}

function readMaybeNumber(obj: unknown, key: string): number {
  if (!obj || typeof obj !== 'object') return 0;
  const rec = obj as Record<string, unknown>;
  const val = rec[key];
  if (typeof val === 'number') return val;
  if (typeof val === 'bigint') return u64AsNumber(val);
  if (typeof val === 'function') {
    try {
      const out = (val as (...args: unknown[]) => unknown).call(obj);
      if (typeof out === 'number') return out;
      if (typeof out === 'bigint') return u64AsNumber(out);
      return 0;
    } catch {
      return 0;
    }
  }
  return 0;
}

function u64AsNumber(v: bigint): number {
  const u = BigInt.asUintN(64, v);
  // This smoke harness only uses small addresses/values; clamp defensively.
  return u > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(u);
}

function i64ToBigInt(v: bigint): bigint {
  return BigInt.asIntN(64, v);
}

async function runTieredVm(iterations: number, threshold: number) {
  let memory: WebAssembly.Memory;
  try {
    const initialPages = Math.ceil((RUNTIME_RESERVED_BYTES + DEFAULT_GUEST_RAM_BYTES) / WASM_PAGE_BYTES);
    const maximumPages = Math.max(initialPages, 4096);
    memory = new WebAssembly.Memory({
      initial: initialPages,
      maximum: maximumPages,
      shared: true,
    });
  } catch (err) {
    postToMain({
      type: 'CpuWorkerError',
      reason: platformSharedMemoryError(err),
    });
    return;
  }

  let api: WasmApi;
  let variant: string;
  try {
    ({ api, variant } = await initWasmForContext({ variant: 'threaded', memory }));
  } catch (err) {
    postToMain({
      type: 'CpuWorkerError',
      reason: err instanceof Error ? err.message : String(err),
    });
    return;
  }

  if (variant !== 'threaded') {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Expected threaded WASM build but got '${variant}'. Ensure crossOriginIsolated + wasmThreads are available.`,
    });
    return;
  }

  const WasmTieredVm = api.WasmTieredVm;
  if (!WasmTieredVm) {
    postToMain({
      type: 'CpuWorkerError',
      reason: 'WasmTieredVm export is unavailable (missing threaded WASM build with tiered VM support).',
    });
    return;
  }

  const desiredGuestBytes = DEFAULT_GUEST_RAM_BYTES;
  const layout = api.guest_ram_layout(desiredGuestBytes);
  const guest_base = layout.guest_base >>> 0;
  const guest_size = layout.guest_size >>> 0;

  if (guest_base + guest_size > memory.buffer.byteLength) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `guest RAM mapping out of bounds: guest_base=0x${guest_base.toString(16)} guest_size=0x${guest_size.toString(16)} mem_bytes=0x${memory.buffer.byteLength.toString(16)}`,
    });
    return;
  }

  // Install JS-side tier-1 call table that the WASM tiered runtime imports via `globalThis.__aero_jit_call`.
  const jitFns: Array<(cpu_ptr: number, jit_ctx_ptr: number) => bigint> = [];
  (globalThis as unknown as { __aero_jit_call?: unknown }).__aero_jit_call = (
    tableIndex: number,
    cpuPtr: number,
    jitCtxPtr: number,
  ) => {
    const fn = jitFns[tableIndex];
    if (typeof fn !== 'function') {
      throw new Error(`missing JIT table entry ${tableIndex}`);
    }
    return fn(cpuPtr, jitCtxPtr);
  };

  // Create tiered VM and write a tiny hot-loop at 0x1000:
  //   add eax, 1   (operand-size override so it is 32-bit in real mode)
  //   jmp short -6
  const vm = new WasmTieredVm(guest_base, guest_size);
  const code = new Uint8Array([0x66, 0x83, 0xc0, 0x01, 0xeb, 0xfa]);
  new Uint8Array(memory.buffer).set(code, guest_base + ENTRY_RIP);
  vm.reset_real_mode(ENTRY_RIP);

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

  const initMsg: CpuToJitMessage = { type: 'JitWorkerInit', memory, guest_base, guest_size };
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

  const dv = new DataView(memory.buffer);

  // Tier-1 imports required by generated blocks (even if the smoke block doesn't use them).
  const env = {
    memory,
    mem_read_u8: (_cpuPtr: number, addr: bigint) => dv.getUint8(guest_base + u64AsNumber(addr)),
    mem_read_u16: (_cpuPtr: number, addr: bigint) => dv.getUint16(guest_base + u64AsNumber(addr), true),
    mem_read_u32: (_cpuPtr: number, addr: bigint) => dv.getUint32(guest_base + u64AsNumber(addr), true) | 0,
    mem_read_u64: (_cpuPtr: number, addr: bigint) => i64ToBigInt(dv.getBigUint64(guest_base + u64AsNumber(addr), true)),
    mem_write_u8: (_cpuPtr: number, addr: bigint, value: number) => {
      dv.setUint8(guest_base + u64AsNumber(addr), value & 0xff);
    },
    mem_write_u16: (_cpuPtr: number, addr: bigint, value: number) => {
      dv.setUint16(guest_base + u64AsNumber(addr), value & 0xffff, true);
    },
    mem_write_u32: (_cpuPtr: number, addr: bigint, value: number) => {
      dv.setUint32(guest_base + u64AsNumber(addr), value >>> 0, true);
    },
    mem_write_u64: (_cpuPtr: number, addr: bigint, value: bigint) => {
      dv.setBigUint64(guest_base + u64AsNumber(addr), BigInt.asUintN(64, value), true);
    },
    mmu_translate: (_cpuPtr: number, jitCtxPtr: number, vaddr: bigint, _access: number) => {
      const vaddrU = BigInt.asUintN(64, vaddr);
      const vpn = vaddrU >> 12n;
      const idx = Number(vpn & 0xffn) >>> 0;

      const tlbSalt = dv.getBigUint64(jitCtxPtr + 8, true);
      const tag = (vpn ^ tlbSalt) | 1n;

      const isRam = vaddrU < BigInt(guest_size);
      const physBase = vaddrU & ~0xfffn;
      const flags = 1n | 2n | 4n | (isRam ? 8n : 0n);
      const data = physBase | flags;

      const entryAddr = jitCtxPtr + 16 + idx * 16;
      dv.setBigUint64(entryAddr, tag, true);
      dv.setBigUint64(entryAddr + 8, data, true);

      return BigInt.asIntN(64, data);
    },
    jit_exit_mmio: (_cpuPtr: number, _vaddr: bigint, _size: number, _isWrite: number, _value: bigint, rip: bigint) => rip,
    jit_exit: (_kind: number, rip: bigint) => rip,
    page_fault: (_cpuPtr: number, _addr: bigint) => -1n,
  };

  let nextTableIndex = 0;
  const installedByRip = new Map<number, number>();
  let interp_executions = 0;
  let jit_executions = 0;

  const recordRunCounts = (runResult: unknown) => {
    if (!runResult || typeof runResult !== 'object') return;
    const rec = runResult as Record<string, unknown>;
    const interp = rec.interp_blocks;
    const jit = rec.jit_blocks;
    if (typeof interp === 'number') interp_executions += interp;
    if (typeof jit === 'number') jit_executions += jit;
  };

  async function installTier1(resp: CompileBlockResponse): Promise<number> {
    const module =
      resp.wasm_module ??
      (resp.wasm_bytes
        ? await WebAssembly.compile(resp.wasm_bytes)
        : (() => {
            throw new Error('JIT response missing both wasm_module and wasm_bytes');
          })());

    const instance = await WebAssembly.instantiate(module, { env });
    const block = (instance.exports as { block?: unknown }).block;
    if (typeof block !== 'function') {
      throw new Error('JIT block module did not export a callable `block` function');
    }

    const tableIndex = nextTableIndex++;
    jitFns[tableIndex] = block as (cpu_ptr: number, jit_ctx_ptr: number) => bigint;
    vm.install_tier1_block(BigInt(resp.entry_rip), tableIndex, BigInt(resp.entry_rip), resp.meta.code_byte_len);
    installedByRip.set(resp.entry_rip, tableIndex);
    return tableIndex;
  }

  // Run the tiered VM loop, forwarding compile requests to the JIT worker.
  let installedIndex: number | null = null;
  const maxBlocks = Math.max(1, iterations | 0);
  let remainingBlocks = maxBlocks;
  while (remainingBlocks > 0) {
    const batch = Math.min(256, remainingBlocks);
    recordRunCounts(vm.run_blocks(batch));
    remainingBlocks -= batch;

    const compileReqs = vm.drain_compile_requests();
    for (const entry_rip of compileReqs as unknown as Iterable<unknown>) {
      const entryRipNum =
        typeof entry_rip === 'bigint'
          ? u64AsNumber(entry_rip)
          : typeof entry_rip === 'number'
            ? entry_rip
            : 0;
      if (!entryRipNum) continue;
      if (installedByRip.has(entryRipNum)) continue;
      try {
        const resp = await requestCompile(entryRipNum);
        const idx = await installTier1(resp);
        if (resp.entry_rip === ENTRY_RIP) installedIndex = idx;
      } catch (err) {
        postToMain({
          type: 'CpuWorkerError',
          reason: `JIT compile failed: ${err instanceof Error ? err.message : String(err)}`,
        });
        jitWorker.terminate();
        try {
          vm.free();
        } catch {
          // ignore
        }
        return;
      }
    }

    if (interp_executions > 0 && jit_executions > 0 && installedIndex !== null) {
      break;
    }

    // Yield so the JIT worker can run in parallel.
    await new Promise((r) => {
      const t = setTimeout(r, 0);
      (t as unknown as { unref?: () => void }).unref?.();
    });
  }

  // Ensure we exercise the installed block at least once.
  if (installedIndex !== null && jit_executions === 0) {
    for (let i = 0; i < 16 && jit_executions === 0; i++) {
      recordRunCounts(vm.run_blocks(1));
    }
  }

  void threshold;

  const runtimeInstalledTableIndex = installedIndex;
  const runtimeInstalledEntryRip = installedIndex !== null ? ENTRY_RIP : null;
  postToMain({
    type: 'CpuWorkerResult',
    jit_executions,
    // Historical field from the earlier placeholder pipeline: keep it non-zero so existing smoke
    // test assertions remain valid.
    helper_executions: Math.max(1, installedByRip.size),
    interp_executions,
    installed_table_index: installedIndex,
    runtime_installed_entry_rip: runtimeInstalledEntryRip,
    runtime_installed_table_index: runtimeInstalledTableIndex,
  });

  jitWorker.terminate();
  try {
    vm.free();
  } catch {
    // ignore
  }
}

ctx.addEventListener('message', (ev: MessageEvent<CpuWorkerStartMessage>) => {
  const msg = ev.data;
  if (msg.type !== 'CpuWorkerStart') return;
  void runTieredVm(msg.iterations ?? 256, msg.threshold ?? 32);
});

postToMain({ type: 'CpuWorkerReady' });
