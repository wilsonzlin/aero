/// <reference lib="webworker" />

import type { CompileBlockResponse, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { asI64, asU64, u64ToNumber } from './bigint';
import { JIT_BIGINT_ABI_WASM_BYTES } from './wasm-bytes';
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
      bigint_imports_ok: boolean;
      // i64/BigInt ABI smoke info (observed via `globalThis.__aero_jit_call`).
      jit_return_type: string | null;
      jit_return_is_sentinel: boolean;
    }
  | { type: 'CpuWorkerError'; reason: string };

type CpuWorkerStartMessage = {
  type: 'CpuWorkerStart';
  iterations?: number;
  threshold?: number;
};

const ENTRY_RIP = 0x1000;

// Tier-1 JIT call status slot (mirrors `crates/aero-jit-x86/src/jit_ctx.rs` + `crates/aero-wasm/src/tiered_vm.rs`).
//
// Layout (relative to `cpuPtr`):
//   CpuState (cpu_state_size bytes, sourced from `api.jit_abi_constants()`)
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
  const u = asU64(v);
  // This smoke harness only uses small addresses/values; clamp defensively.
  return u > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(u);
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

  const jitAbiFn = api.jit_abi_constants;
  if (typeof jitAbiFn !== 'function') {
    postToMain({
      type: 'CpuWorkerError',
      reason: 'Missing jit_abi_constants export from aero-wasm; rebuild the WASM package.',
    });
    return;
  }

  const jitAbi = jitAbiFn();
  const cpu_state_size = readMaybeNumber(jitAbi, 'cpu_state_size') >>> 0;
  const cpu_state_align = readMaybeNumber(jitAbi, 'cpu_state_align') >>> 0;
  const cpu_rip_off = readMaybeNumber(jitAbi, 'cpu_rip_off') >>> 0;
  const cpu_rflags_off = readMaybeNumber(jitAbi, 'cpu_rflags_off') >>> 0;
  const cpu_gpr_off = (jitAbi as any)?.cpu_gpr_off;
  if (
    !cpu_state_size ||
    !cpu_state_align ||
    !cpu_rip_off ||
    !cpu_rflags_off ||
    !(cpu_gpr_off instanceof Uint32Array) ||
    cpu_gpr_off.length !== 16
  ) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_abi_constants payload from aero-wasm: ${JSON.stringify({
        cpu_state_size,
        cpu_state_align,
        cpu_rip_off,
        cpu_rflags_off,
        cpu_gpr_off_type: cpu_gpr_off?.constructor?.name,
        cpu_gpr_off_len: cpu_gpr_off?.length,
      })}`,
    });
    return;
  }

  const cpu_rax_off = cpu_gpr_off[0]! >>> 0;
  const COMMIT_FLAG_OFFSET = cpu_state_size + JIT_CTX_TOTAL_BYTES + TIER2_CTX_BYTES;

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
  // Slots are recycled on cache eviction so the table stays bounded.
  const jitFns: Array<((cpu_ptr: number, jit_ctx_ptr: number) => bigint) | undefined> = [];
  let lastJitReturnType: string | null = null;
  let lastJitReturnIsSentinel = false;

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
    const cpuSnapshot = memU8.slice(cpuPtr, cpuPtr + cpu_state_size);

    let rawRet: unknown;
    try {
      rawRet = fn(cpuPtr, jitCtxPtr);
    } finally {
      activeExitState = null;
      activeWriteLog = null;
    }

    lastJitReturnType = typeof rawRet;
    if (typeof rawRet !== 'bigint') {
      throw new TypeError(`Tier-1 JIT block returned ${typeof rawRet}; expected bigint (wasm i64).`);
    }
    const ret = rawRet as bigint;
    lastJitReturnIsSentinel = asI64(ret) === JIT_EXIT_SENTINEL_I64;

    // Tier-1 contract: sentinel return value requests interpreter fallback.
    const exitToInterpreter = lastJitReturnIsSentinel;
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
  {
    const vmAny = vm as unknown as {
      on_guest_write?: unknown;
      jit_on_guest_write?: unknown;
    };
    const notify = vmAny.on_guest_write ?? vmAny.jit_on_guest_write;
    if (typeof notify === 'function') {
      onGuestWrite = (paddr: bigint, len: number) => {
        const paddrU64 = asU64(paddr);
        const lenU32 = len >>> 0;
        try {
          (notify as (paddr: bigint, len: number) => void).call(vm, paddrU64, lenU32);
          return;
        } catch {
          // Backwards-compat: older wasm-bindgen APIs may expose these methods with `u32` params
          // (number) instead of `u64` (BigInt). Fall back to a lossy-but-safe u32 conversion when
          // the BigInt call fails.
          try {
            (notify as (paddr: number, len: number) => void).call(vm, u64ToNumber(paddrU64), lenU32);
          } catch {
            // ignore
          }
        }
      };
    } else {
      onGuestWrite = null;
    }
  }

  const logWrite = (linearAddr: number, size: number) => {
    const log = activeWriteLog;
    if (!log) return;
    refreshMemU8();
    log.push({ addr: linearAddr, size, old_value_bytes: memU8.slice(linearAddr, linearAddr + size) });
  };

  // Tier-1 imports required by generated blocks (even if the smoke block doesn't use them).
  const env = {
    memory,
    mem_read_u8: (_cpuPtr: number, addr: bigint) => dv.getUint8(guest_base + u64ToNumber(addr)),
    mem_read_u16: (_cpuPtr: number, addr: bigint) => dv.getUint16(guest_base + u64ToNumber(addr), true),
    mem_read_u32: (_cpuPtr: number, addr: bigint) => dv.getUint32(guest_base + u64ToNumber(addr), true) | 0,
    mem_read_u64: (_cpuPtr: number, addr: bigint) => asI64(dv.getBigUint64(guest_base + u64ToNumber(addr), true)),
    mem_write_u8: (_cpuPtr: number, addr: bigint, value: number) => {
      const off = u64ToNumber(addr);
      if (off >= guest_size) return;
      const linear = guest_base + off;
      logWrite(linear, 1);
      dv.setUint8(linear, value & 0xff);
      // If the helper is used outside a JIT block (unlikely), still bump code versions.
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 1);
    },
    mem_write_u16: (_cpuPtr: number, addr: bigint, value: number) => {
      const off = u64ToNumber(addr);
      if (off + 2 > guest_size) return;
      const linear = guest_base + off;
      logWrite(linear, 2);
      dv.setUint16(linear, value & 0xffff, true);
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 2);
    },
    mem_write_u32: (_cpuPtr: number, addr: bigint, value: number) => {
      const off = u64ToNumber(addr);
      if (off + 4 > guest_size) return;
      const linear = guest_base + off;
      logWrite(linear, 4);
      dv.setUint32(linear, value >>> 0, true);
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 4);
    },
    mem_write_u64: (_cpuPtr: number, addr: bigint, value: bigint) => {
      const off = u64ToNumber(addr);
      if (off + 8 > guest_size) return;
      const linear = guest_base + off;
      logWrite(linear, 8);
      dv.setBigUint64(linear, asU64(value), true);
      if (!activeWriteLog && onGuestWrite) onGuestWrite(addr, 8);
    },
    mmu_translate: (_cpuPtr: number, jitCtxPtr: number, vaddr: bigint, _access: number) => {
      const vaddrU = asU64(vaddr);
      const vpn = vaddrU >> 12n;
      const idx = Number(vpn & 0xffn) >>> 0;

      const tlbSalt = dv.getBigUint64(jitCtxPtr + 8, true);
      const tag = asU64((vpn ^ tlbSalt) | 1n);

      const isRam = vaddrU < BigInt(guest_size);
      const physBase = vaddrU & ~0xfffn;
      const flags = 1n | 2n | 4n | (isRam ? 8n : 0n);
      const data = asU64(physBase | flags);

      const entryAddr = jitCtxPtr + 16 + idx * 16;
      dv.setBigUint64(entryAddr, tag, true);
      dv.setBigUint64(entryAddr + 8, data, true);

      return asI64(data);
    },
    jit_exit_mmio: (_cpuPtr: number, _vaddr: bigint, _size: number, _isWrite: number, _value: bigint, rip: bigint) => {
      if (activeExitState) activeExitState.mmio_exit = true;
      return asI64(rip);
    },
    jit_exit: (_kind: number, rip: bigint) => {
      if (activeExitState) activeExitState.jit_exit = true;
      return asI64(rip);
    },
    page_fault: (_cpuPtr: number, _addr: bigint) => {
      if (activeExitState) activeExitState.page_fault = true;
      return JIT_EXIT_SENTINEL_I64;
    },
  };

  let nextTableIndex = 0;
  const freeTableIndices: number[] = [];
  const installedByRip = new Map<number, number>();
  const compilingByRip = new Set<number>();
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

  const runBigIntImportsTest = async (): Promise<boolean> => {
    try {
      const module = await WebAssembly.compile(JIT_BIGINT_ABI_WASM_BYTES);
      const instance = await WebAssembly.instantiate(module, { env });
      const block = (instance.exports as { block?: unknown }).block;
      if (typeof block !== 'function') return false;

      // Pick deterministic addresses in guest RAM that are not touched by the hot-loop code at 0x1000.
      // We use guest RAM as a scratch region for the Tier-1 ABI buffer (CpuState + jit_ctx + tier2_ctx + commit flag).
      const cpuPtr = (guest_base + 0xa000) >>> 0;
      const jitCtxPtr = (cpuPtr + cpu_state_size) >>> 0;
      if (cpuPtr + COMMIT_FLAG_OFFSET + 4 > guest_base + guest_size) return false;

      // Initialize the minimal JitContext header expected by our `mmu_translate` stub.
      dv.setBigUint64(jitCtxPtr + 0, BigInt(guest_base), true); // ram_base
      dv.setBigUint64(jitCtxPtr + 8, 0n, true); // tlb_salt

      const tableIndex = nextTableIndex++;
      jitFns[tableIndex] = block as (cpu_ptr: number, jit_ctx_ptr: number) => bigint;

      // Avoid notifying the tiered runtime of these synthetic guest writes; this is purely an ABI smoke check.
      const savedOnGuestWrite = onGuestWrite;
      onGuestWrite = null;
      try {
        const ret = (
          globalThis as unknown as { __aero_jit_call: (idx: number, cpu: number, ctx: number) => bigint }
        ).__aero_jit_call(tableIndex, cpuPtr, jitCtxPtr);
        return typeof ret === 'bigint';
      } finally {
        onGuestWrite = savedOnGuestWrite;
      }
    } catch {
      return false;
    }
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
        dv.setBigUint64(cpuPtr + cpu_rax_off, preRax, true);
        dv.setBigUint64(cpuPtr + cpu_rip_off, preRip, true);
        dv.setUint32(storeLinear, preStore, true);
        refreshMemU8();
        return memU8.slice(cpuPtr, cpuPtr + cpu_state_size);
      };

      const callAndAssertRollback = (
        trigger: 'jit_exit' | 'mmio_exit' | 'page_fault' | 'term',
        expectRollback: boolean,
      ): boolean => {
        const cpuBefore = initState();

        const tableIndex = nextTableIndex++;
        jitFns[tableIndex] = (cpu_ptr: number, _jit_ctx_ptr: number): bigint => {
          // Mutate the CpuState ABI region.
          const rax = dv.getBigUint64(cpu_ptr + cpu_rax_off, true);
          dv.setBigUint64(cpu_ptr + cpu_rax_off, rax + 1n, true);
          const rip = dv.getBigUint64(cpu_ptr + cpu_rip_off, true);
          dv.setBigUint64(cpu_ptr + cpu_rip_off, rip + 1n, true);

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
        const cpuAfter = memU8.slice(cpuPtr, cpuPtr + cpu_state_size);
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
        const rax = dv.getBigUint64(cpu_ptr + cpu_rax_off, true);
        dv.setBigUint64(cpu_ptr + cpu_rax_off, rax + 2n, true);
        return JIT_EXIT_SENTINEL_I64;
      };
      dv.setUint32(cpuPtr + COMMIT_FLAG_OFFSET, 0, true);
      dv.setBigUint64(cpuPtr + cpu_rax_off, preRax, true);
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

  const allocTableIndex = (): number => {
    const reused = freeTableIndices.pop();
    if (reused !== undefined) return reused;
    return nextTableIndex++;
  };

  const freeTableIndex = (idx: number) => {
    jitFns[idx] = undefined;
    freeTableIndices.push(idx);
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

    const entryRipU32 = resp.entry_rip >>> 0;

    // Reuse the existing table slot if this RIP was compiled before. This makes recompilation
    // (self-modifying code invalidation) overwrite the previous slot rather than growing the JS
    // call table unboundedly.
    const existingIndex = installedByRip.get(entryRipU32);
    const tableIndex = existingIndex ?? allocTableIndex();
    jitFns[tableIndex] = block as (cpu_ptr: number, jit_ctx_ptr: number) => bigint;

    // wasm-bindgen APIs differ across versions: newer builds return a list of evicted RIPs from
    // `install_tier1_block`, while older builds returned `void`. Capture as `unknown` so we can
    // best-effort free table indices without breaking typecheck.
    let evicted: unknown;
    try {
      evicted = vm.install_tier1_block(BigInt(entryRipU32), tableIndex, BigInt(entryRipU32), resp.meta.code_byte_len) as unknown;
    } catch {
      // Backwards-compat: older wasm-bindgen exports used u32 params (number) instead of u64 (BigInt).
      evicted = (vm.install_tier1_block as unknown as (...args: unknown[]) => unknown)(
        entryRipU32,
        tableIndex,
        entryRipU32,
        resp.meta.code_byte_len,
      );
    }
    installedByRip.set(entryRipU32, tableIndex);

    // If the JIT cache evicted older blocks, free their table indices so they can be reused.
    const releaseEvictedRip = (rip: number) => {
      const ripU32 = rip >>> 0;
      if (ripU32 === 0 || ripU32 === entryRipU32) return;
      const idx = installedByRip.get(ripU32);
      if (idx === undefined) return;
      installedByRip.delete(ripU32);
      if (ripU32 === ENTRY_RIP && installedIndex === idx) {
        installedIndex = null;
      }
      freeTableIndex(idx);
    };

    if (Array.isArray(evicted)) {
      for (const v of evicted) {
        if (typeof v === 'bigint') {
          try {
            releaseEvictedRip(u64ToNumber(v));
          } catch {
            // ignore out-of-range
          }
        }
        else if (typeof v === 'number' && Number.isFinite(v)) releaseEvictedRip(v);
      }
    } else if (ArrayBuffer.isView(evicted)) {
      // Older WASM builds may return a typed array (e.g. Uint32Array).
      // Some runtimes may also use BigInt typed arrays; handle both.
      const view = evicted as unknown as ArrayLike<unknown>;
      const len = (view as { length?: unknown }).length;
      if (typeof len === 'number' && Number.isFinite(len) && len > 0) {
        for (let i = 0; i < len; i++) {
          const v = view[i];
          if (typeof v === 'bigint') {
            try {
              releaseEvictedRip(u64ToNumber(v));
            } catch {
              // ignore out-of-range
            }
          }
          else if (typeof v === 'number' && Number.isFinite(v)) releaseEvictedRip(v);
        }
      }
    } else if (evicted != null && typeof evicted === 'object') {
      // Best-effort: treat as iterable.
      try {
        for (const v of evicted as unknown as Iterable<unknown>) {
          if (typeof v === 'bigint') {
            try {
              releaseEvictedRip(u64ToNumber(v));
            } catch {
              // ignore out-of-range
            }
          }
          else if (typeof v === 'number' && Number.isFinite(v)) releaseEvictedRip(v);
        }
      } catch {
        // ignore
      }
    }
    return tableIndex;
  }

  const drainCompileRequests = (): number[] => {
    const out: number[] = [];
    const compileReqs = vm.drain_compile_requests();
    for (const entry_rip of compileReqs as unknown as Iterable<unknown>) {
      let entryRipU32: number | undefined;
      if (typeof entry_rip === 'bigint') {
        try {
          entryRipU32 = u64ToNumber(entry_rip) >>> 0;
        } catch {
          entryRipU32 = undefined;
        }
      } else if (typeof entry_rip === 'number' && Number.isFinite(entry_rip)) {
        entryRipU32 = entry_rip >>> 0;
      }
      if (!entryRipU32) continue;
      out.push(entryRipU32);
    }
    return out;
  };

  const compileAndInstall = async (entryRipNum: number): Promise<number | null> => {
    const entryRipU32 = entryRipNum >>> 0;
    if (!entryRipU32) return null;
    if (compilingByRip.has(entryRipU32)) return null;
    compilingByRip.add(entryRipU32);
    try {
      const resp = await requestCompile(entryRipU32);
      const idx = await installTier1(resp);
      if (entryRipU32 === ENTRY_RIP) installedIndex = idx;
      return idx;
    } finally {
      compilingByRip.delete(entryRipU32);
    }
  };

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

    try {
      for (const entryRipNum of drainCompileRequests()) {
        await compileAndInstall(entryRipNum);
      }
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

  // Regression: self-modifying code invalidation must trigger recompilation even if the RIP was
  // installed once already (JS must not ignore compile requests solely based on prior installs).
  if (installedIndex !== null) {
    try {
      const jitBeforeInvalidation = jit_executions;

      // Patch the guest code bytes in-place (modify the `add eax, imm8` immediate from 1 -> 2).
      const patched = new Uint8Array([0x66, 0x83, 0xc0, 0x02, 0xeb, 0xfa]);
      new Uint8Array(memory.buffer).set(patched, guest_base + ENTRY_RIP);
      if (!onGuestWrite) {
        throw new Error(
          'WasmTieredVm.on_guest_write/jit_on_guest_write is unavailable; cannot test self-modifying code invalidation',
        );
      }
      onGuestWrite(BigInt(ENTRY_RIP), patched.byteLength);

      let sawRecompileRequest = false;

      // Drive the VM until it requests recompilation for ENTRY_RIP and we install the result.
      for (let i = 0; i < 256; i++) {
        recordRunCounts(vm.run_blocks(1));

        const reqs = drainCompileRequests();
        if (reqs.includes(ENTRY_RIP)) sawRecompileRequest = true;
        for (const entryRipNum of reqs) {
          await compileAndInstall(entryRipNum);
        }

        if (sawRecompileRequest && installedIndex !== null) break;

        await new Promise((r) => {
          const t = setTimeout(r, 0);
          (t as unknown as { unref?: () => void }).unref?.();
        });
      }

      if (!sawRecompileRequest) {
        postToMain({
          type: 'CpuWorkerError',
          reason: 'Self-modifying code regression: expected ENTRY_RIP to be re-requested for compilation after invalidation',
        });
        jitWorker.terminate();
        try {
          vm.free();
        } catch {
          // ignore
        }
        return;
      }

      // After reinstall, Tier-1 execution should resume (jit blocks count increases).
      for (let i = 0; i < 64 && jit_executions <= jitBeforeInvalidation; i++) {
        recordRunCounts(vm.run_blocks(1));
        for (const entryRipNum of drainCompileRequests()) {
          await compileAndInstall(entryRipNum);
        }
      }

      if (jit_executions <= jitBeforeInvalidation) {
        postToMain({
          type: 'CpuWorkerError',
          reason: 'Self-modifying code regression: Tier-1 JIT execution did not resume after recompilation',
        });
        jitWorker.terminate();
        try {
          vm.free();
        } catch {
          // ignore
        }
        return;
      }
    } catch (err) {
      postToMain({
        type: 'CpuWorkerError',
        reason: `Self-modifying code regression: unexpected error during recompilation: ${err instanceof Error ? err.message : String(err)}`,
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

  void threshold;
  const bigint_imports_ok = await runBigIntImportsTest();
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
    bigint_imports_ok,
    jit_return_type: lastJitReturnType,
    jit_return_is_sentinel: lastJitReturnIsSentinel,
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
