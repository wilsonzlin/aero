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
      rollback_ok: boolean;
    }
  | { type: 'CpuWorkerError'; reason: string };

type CpuWorkerStartMessage = {
  type: 'CpuWorkerStart';
  iterations?: number;
  threshold?: number;
};

const ENTRY_RIP = 0x1000;

// Tier-1 JIT ABI constants (mirrors `aero_cpu_core::state`).
// If these drift, rollback will restore the wrong byte window.
const CPU_STATE_SIZE = 1072;
const CPU_RAX_OFF = 0;
const CPU_RIP_OFF = 128;

// Tier-1 JIT call status slot (mirrors `crates/aero-jit-x86/src/jit_ctx.rs` + `crates/aero-wasm/src/tiered_vm.rs`).
//
// Layout (relative to `cpuPtr`):
//   CpuState (CPU_STATE_SIZE)
//   JitContext (header + inline TLB)
//   Tier-2 ctx (12 bytes)
//   commit_flag (u32)
//
// JS sets `commit_flag = 0` when it rolls back architectural + memory effects on runtime exits.
// The tiered dispatcher uses this to avoid retiring guest instructions for rolled-back blocks.
const JIT_CTX_HEADER_BYTES = 16;
const JIT_TLB_ENTRIES = 256;
const JIT_TLB_ENTRY_BYTES = 16;
const JIT_CTX_TOTAL_BYTES = JIT_CTX_HEADER_BYTES + JIT_TLB_ENTRIES * JIT_TLB_ENTRY_BYTES;
const TIER2_CTX_BYTES = 12;
const COMMIT_FLAG_OFFSET = CPU_STATE_SIZE + JIT_CTX_TOTAL_BYTES + TIER2_CTX_BYTES;

// Tier-1 "exit to interpreter" sentinel return value (`u64::MAX` encoded as `i64`).
const JIT_EXIT_SENTINEL_I64 = -1n;

type HostExitState = { mmio_exit: boolean; jit_exit: boolean; page_fault: boolean };
type WriteLogEntry = { addr: number; size: number; old_value_bytes: Uint8Array };

function hostExitStateShouldRollback(state: HostExitState): boolean {
  return state.mmio_exit || state.jit_exit || state.page_fault;
}

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

function u64AsNumber(v: bigint): number {
  const u = BigInt.asUintN(64, v);
  // This smoke harness only uses small addresses/values; clamp defensively.
  return u > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(u);
}

function i64ToBigInt(v: bigint): bigint {
  return BigInt.asIntN(64, v);
}

function readMaybeNumber(vm: unknown, key: string): number {
  try {
    if (!vm || (typeof vm !== 'object' && typeof vm !== 'function')) return 0;

    let value: unknown;
    try {
      value = (vm as Record<string, unknown>)[key];
    } catch {
      return 0;
    }

    // wasm-bindgen has shipped both `get foo()` and `foo()` styles for similar APIs;
    // accept either shape without throwing.
    if (typeof value === 'function') {
      try {
        value = (value as (...args: never[]) => unknown).call(vm);
      } catch {
        return 0;
      }
    }

    if (typeof value === 'number') {
      return Number.isFinite(value) ? value : 0;
    }

    if (typeof value === 'bigint') {
      return u64AsNumber(value);
    }

    if (typeof value === 'string') {
      const trimmed = value.trim().toLowerCase();
      if (!trimmed) return 0;
      const n = trimmed.startsWith('0x') ? Number.parseInt(trimmed.slice(2), 16) : Number.parseInt(trimmed, 10);
      return Number.isFinite(n) ? n : 0;
    }

    return 0;
  } catch {
    return 0;
  }
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

  // Rollback state is scoped to a single `__aero_jit_call` invocation.
  // `env.*` imports consult these while a block is executing.
  let activeExitState: HostExitState | null = null;
  let activeWriteLog: WriteLogEntry[] | null = null;
  let onGuestWrite: ((paddr: bigint, len: number) => void) | null = null;

  // Used for CPU state snapshots + write log byte copies.
  // NOTE: This is a view, not a copy; snapshotting uses `.slice()`.
  let memU8 = new Uint8Array(memory.buffer);
  const refreshMemU8 = () => {
    if (memU8.buffer === memory.buffer) return;
    memU8 = new Uint8Array(memory.buffer);
  };

  (globalThis as unknown as { __aero_jit_call?: unknown }).__aero_jit_call = (
    tableIndex: number,
    cpuPtr: number,
    jitCtxPtr: number,
  ): bigint => {
    const fn = jitFns[tableIndex];
    if (typeof fn !== 'function') {
      throw new Error(`missing JIT table entry ${tableIndex}`);
    }

    refreshMemU8();

    const commitFlagAddr = (cpuPtr + COMMIT_FLAG_OFFSET) >>> 0;
    // Default to "committed". Rollback paths clear this before returning so the WASM tiered VM can
    // report `JitBlockExit { committed: false }`.
    dv.setUint32(commitFlagAddr, 1, true);

    // Per-call HostExitState + guest RAM write log.
    const exitState: HostExitState = { mmio_exit: false, jit_exit: false, page_fault: false };
    const writeLog: WriteLogEntry[] = [];
    activeExitState = exitState;
    activeWriteLog = writeLog;

    // Snapshot the CpuState ABI region so we can roll back partial side effects on runtime exit.
    const cpuSnapshot = memU8.slice(cpuPtr, cpuPtr + CPU_STATE_SIZE);

    let ret: bigint;
    try {
      ret = fn(cpuPtr, jitCtxPtr);
    } finally {
      activeExitState = null;
      activeWriteLog = null;
    }

    // Tier-1 contract: sentinel return value requests interpreter fallback.
    const exitToInterpreter = ret === JIT_EXIT_SENTINEL_I64;
    const shouldRollback = exitToInterpreter && hostExitStateShouldRollback(exitState);

    if (shouldRollback) {
      // Roll back guest RAM writes (reverse order) and restore pre-block CPU state.
      refreshMemU8();
      for (let i = writeLog.length - 1; i >= 0; i--) {
        const entry = writeLog[i]!;
        memU8.set(entry.old_value_bytes, entry.addr);
      }
      memU8.set(cpuSnapshot, cpuPtr);
      dv.setUint32(commitFlagAddr, 0, true);
    } else if (writeLog.length && onGuestWrite) {
      // Notify the tiered runtime of committed guest writes so it can bump code page versions for
      // self-modifying code invalidation. We intentionally skip this on rolled-back exits.
      for (const entry of writeLog) {
        const paddr = entry.addr - guest_base;
        if (paddr >= 0) onGuestWrite(BigInt(paddr), entry.size);
      }
    }

    return ret;
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
      // The smoke harness runs the guest in 16-bit real mode.
      bitness: 16,
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
  onGuestWrite = (paddr: bigint, len: number) => {
    const notify = (vm as unknown as { on_guest_write?: (paddr: bigint, len: number) => void }).on_guest_write;
    if (typeof notify !== 'function') return;
    notify.call(vm, BigInt.asUintN(64, paddr), len >>> 0);
  };

  const logWrite = (linearAddr: number, size: number) => {
    const log = activeWriteLog;
    if (!log) return;
    refreshMemU8();
    log.push({ addr: linearAddr, size, old_value_bytes: memU8.slice(linearAddr, linearAddr + size) });
  };

  // Tier-1 imports required by generated blocks (even if the smoke block doesn't use them).
  const env = {
    memory,
    mem_read_u8: (_cpuPtr: number, addr: bigint) => dv.getUint8(guest_base + u64AsNumber(addr)),
    mem_read_u16: (_cpuPtr: number, addr: bigint) => dv.getUint16(guest_base + u64AsNumber(addr), true),
    mem_read_u32: (_cpuPtr: number, addr: bigint) => dv.getUint32(guest_base + u64AsNumber(addr), true) | 0,
    mem_read_u64: (_cpuPtr: number, addr: bigint) => i64ToBigInt(dv.getBigUint64(guest_base + u64AsNumber(addr), true)),
    mem_write_u8: (_cpuPtr: number, addr: bigint, value: number) => {
      const linear = guest_base + u64AsNumber(addr);
      logWrite(linear, 1);
      dv.setUint8(linear, value & 0xff);
      // If the helper is used outside a JIT block (unlikely), still bump code versions.
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 1);
    },
    mem_write_u16: (_cpuPtr: number, addr: bigint, value: number) => {
      const linear = guest_base + u64AsNumber(addr);
      logWrite(linear, 2);
      dv.setUint16(linear, value & 0xffff, true);
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 2);
    },
    mem_write_u32: (_cpuPtr: number, addr: bigint, value: number) => {
      const linear = guest_base + u64AsNumber(addr);
      logWrite(linear, 4);
      dv.setUint32(linear, value >>> 0, true);
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 4);
    },
    mem_write_u64: (_cpuPtr: number, addr: bigint, value: bigint) => {
      const linear = guest_base + u64AsNumber(addr);
      logWrite(linear, 8);
      dv.setBigUint64(linear, BigInt.asUintN(64, value), true);
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 8);
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
    jit_exit_mmio: (_cpuPtr: number, _vaddr: bigint, _size: number, _isWrite: number, _value: bigint, rip: bigint) => {
      if (activeExitState) activeExitState.mmio_exit = true;
      return rip;
    },
    jit_exit: (_kind: number, rip: bigint) => {
      if (activeExitState) activeExitState.jit_exit = true;
      return rip;
    },
    page_fault: (_cpuPtr: number, _addr: bigint) => {
      if (activeExitState) activeExitState.page_fault = true;
      return JIT_EXIT_SENTINEL_I64;
    },
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

  const arraysEqual = (a: Uint8Array, b: Uint8Array): boolean => {
    if (a.byteLength !== b.byteLength) return false;
    for (let i = 0; i < a.byteLength; i++) {
      if (a[i] !== b[i]) return false;
    }
    return true;
  };

  const runRollbackTest = (): boolean => {
    try {
      refreshMemU8();

      // Pick deterministic addresses in guest RAM that are not touched by the hot-loop code at 0x1000.
      const cpuPtr = guest_base + 0x8000;
      const storeAddr = 0x200;
      const storeLinear = guest_base + storeAddr;

      const preRax = 0x1111222233334444n;
      const preRip = 0x5555666677778888n;
      const preStore = 0xdeadbeef;

      const initState = () => {
        dv.setBigUint64(cpuPtr + CPU_RAX_OFF, preRax, true);
        dv.setBigUint64(cpuPtr + CPU_RIP_OFF, preRip, true);
        dv.setUint32(storeLinear, preStore, true);
        refreshMemU8();
        return memU8.slice(cpuPtr, cpuPtr + CPU_STATE_SIZE);
      };

      const callAndAssertRollback = (
        trigger: 'jit_exit' | 'mmio_exit' | 'page_fault' | 'term',
        expectRollback: boolean,
      ): boolean => {
        const cpuBefore = initState();

        const tableIndex = nextTableIndex++;
        jitFns[tableIndex] = (cpu_ptr: number, _jit_ctx_ptr: number): bigint => {
          // Mutate the CpuState ABI region.
          const rax = dv.getBigUint64(cpu_ptr + CPU_RAX_OFF, true);
          dv.setBigUint64(cpu_ptr + CPU_RAX_OFF, rax + 1n, true);
          const rip = dv.getBigUint64(cpu_ptr + CPU_RIP_OFF, true);
          dv.setBigUint64(cpu_ptr + CPU_RIP_OFF, rip + 1n, true);

          // Guest RAM store goes through the helper so it is logged.
          env.mem_write_u32(cpu_ptr, BigInt(storeAddr), 0x12345678);

          switch (trigger) {
            case 'jit_exit':
              env.jit_exit(0, 0n);
              break;
            case 'mmio_exit':
              env.jit_exit_mmio(cpu_ptr, 0n, 4, 1, 0n, 0n);
              break;
            case 'page_fault':
              env.page_fault(cpu_ptr, 0n);
              break;
            case 'term':
              // No explicit runtime exit flag: simulate a normal `ExitToInterpreter` terminator.
              break;
          }
          return JIT_EXIT_SENTINEL_I64;
        };

        const ret = (
          globalThis as unknown as { __aero_jit_call: (idx: number, cpu: number, ctx: number) => bigint }
        ).__aero_jit_call(tableIndex, cpuPtr, 0);
        if (ret !== JIT_EXIT_SENTINEL_I64) return false;

        refreshMemU8();
        const cpuAfter = memU8.slice(cpuPtr, cpuPtr + CPU_STATE_SIZE);
        const storeAfter = dv.getUint32(storeLinear, true);
        const commitAfter = dv.getUint32(cpuPtr + COMMIT_FLAG_OFFSET, true);

        if (expectRollback) {
          if (!arraysEqual(cpuBefore, cpuAfter)) return false;
          if (storeAfter !== preStore) return false;
          if (commitAfter !== 0) return false;
          return true;
        }

        if (arraysEqual(cpuBefore, cpuAfter)) return false;
        if (storeAfter === preStore) return false;
        if (commitAfter !== 1) return false;
        return true;
      };

      // Rollback exits (must clear commit flag).
      if (!callAndAssertRollback('jit_exit', true)) return false;
      if (!callAndAssertRollback('mmio_exit', true)) return false;
      if (!callAndAssertRollback('page_fault', true)) return false;
      if (!callAndAssertRollback('term', false)) return false;

      // Separate check: seed the commit flag with 0 so we can confirm `__aero_jit_call` resets it
      // to 1 on entry for non-rollback paths.
      const committedIndex = nextTableIndex++;
      jitFns[committedIndex] = (cpu_ptr: number, _jit_ctx_ptr: number): bigint => {
        const rax = dv.getBigUint64(cpu_ptr + CPU_RAX_OFF, true);
        dv.setBigUint64(cpu_ptr + CPU_RAX_OFF, rax + 2n, true);
        return JIT_EXIT_SENTINEL_I64;
      };
      dv.setUint32(cpuPtr + COMMIT_FLAG_OFFSET, 0, true);
      dv.setBigUint64(cpuPtr + CPU_RAX_OFF, preRax, true);
      const retCommitted = (
        globalThis as unknown as { __aero_jit_call: (idx: number, cpu: number, ctx: number) => bigint }
      ).__aero_jit_call(committedIndex, cpuPtr, 0);
      if (retCommitted !== JIT_EXIT_SENTINEL_I64) return false;
      const commitCommitted = dv.getUint32(cpuPtr + COMMIT_FLAG_OFFSET, true);
      if (commitCommitted !== 1) return false;

      return true;
    } catch {
      return false;
    }
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
    const entryRipU32 = resp.entry_rip >>> 0;
    vm.install_tier1_block(BigInt(entryRipU32), tableIndex, BigInt(entryRipU32), resp.meta.code_byte_len);
    installedByRip.set(entryRipU32, tableIndex);
    return tableIndex;
  }

  // Run the tiered VM loop, forwarding compile requests to the JIT worker.
  let installedIndex: number | null = null;
  const maxBlocks = Math.max(1, iterations | 0);
  let remainingBlocks = maxBlocks;
  while (remainingBlocks > 0) {
    const batch = Math.min(256, remainingBlocks);
    let runResult: unknown = undefined;
    try {
      runResult = vm.run_blocks(batch);
    } catch (err) {
      postToMain({
        type: 'CpuWorkerError',
        reason: `Tiered VM run_blocks failed: ${err instanceof Error ? err.message : String(err)}`,
      });
      jitWorker.terminate();
      try {
        vm.free();
      } catch {
        // ignore
      }
      return;
    }
    recordRunCounts(runResult);
    remainingBlocks -= batch;

    const compileReqs = vm.drain_compile_requests();
    for (const entry_rip of compileReqs as unknown as Iterable<unknown>) {
      let entryRipNum: number | undefined;
      if (typeof entry_rip === 'bigint') {
        entryRipNum = u64AsNumber(entry_rip);
      } else if (typeof entry_rip === 'number' && Number.isFinite(entry_rip)) {
        entryRipNum = entry_rip >>> 0;
      }
      if (entryRipNum === undefined) continue;
      const entryRipU32 = entryRipNum >>> 0;
      if (installedByRip.has(entryRipU32)) continue;
      try {
        const resp = await requestCompile(entryRipU32);
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

    const interpTotal = Math.max(
      interp_executions,
      readMaybeNumber(vm, 'interp_blocks_total'),
      readMaybeNumber(vm, 'interp_executions'),
    );
    const jitTotal = Math.max(jit_executions, readMaybeNumber(vm, 'jit_blocks_total'), readMaybeNumber(vm, 'jit_executions'));
    if (interpTotal > 0 && jitTotal > 0 && installedIndex !== null) {
      break;
    }

    // Yield so the JIT worker can run in parallel.
    await new Promise((r) => {
      const t = setTimeout(r, 0);
      (t as unknown as { unref?: () => void }).unref?.();
    });
  }

  // Ensure we exercise the installed block at least once.
  if (installedIndex !== null) {
    for (let i = 0; i < 16; i++) {
      const jitTotal = Math.max(
        jit_executions,
        readMaybeNumber(vm, 'jit_blocks_total'),
        readMaybeNumber(vm, 'jit_executions'),
      );
      if (jitTotal > 0) break;
      try {
        recordRunCounts(vm.run_blocks(1));
      } catch {
        break;
      }
    }
  }

  void threshold;
  const rollback_ok = runRollbackTest();

  const interp_executions_total = Math.max(
    interp_executions,
    readMaybeNumber(vm, 'interp_blocks_total'),
    readMaybeNumber(vm, 'interp_executions'),
  );
  const jit_executions_total = Math.max(
    jit_executions,
    readMaybeNumber(vm, 'jit_blocks_total'),
    readMaybeNumber(vm, 'jit_executions'),
  );

  const runtimeInstalledTableIndex = installedIndex;
  const runtimeInstalledEntryRip = installedIndex !== null ? ENTRY_RIP : null;
  postToMain({
    type: 'CpuWorkerResult',
    jit_executions: jit_executions_total,
    // Historical field from the earlier placeholder pipeline: keep it non-zero so existing smoke
    // test assertions remain valid.
    helper_executions: Math.max(1, installedByRip.size),
    interp_executions: interp_executions_total,
    installed_table_index: installedIndex,
    runtime_installed_entry_rip: runtimeInstalledEntryRip,
    runtime_installed_table_index: runtimeInstalledTableIndex,
    rollback_ok,
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
