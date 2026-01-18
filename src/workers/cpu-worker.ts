/// <reference lib="webworker" />

import type { CompileBlockResponse, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { asI64, asU64, u64ToNumber } from './bigint';
import { JIT_BIGINT_ABI_WASM_BYTES, JIT_CODE_PAGE_VERSION_ABI_WASM_BYTES } from './wasm-bytes';
import { HIGH_RAM_START as HIGH_RAM_START_U53, LOW_RAM_END as LOW_RAM_END_U32 } from '../../web/src/arch/guest_phys.ts';
import { initWasmForContext, type WasmApi } from '../../web/src/runtime/wasm_context';
import { formatOneLineError } from '../text.js';
import { unrefBestEffort } from '../unref_safe.js';

declare global {
  // Tiered VM bus exit hooks installed by this worker for deterministic JIT smoke tests.
  // eslint-disable-next-line no-var
  var __aero_io_port_read: ((port: number, size: number) => number) | undefined;
  // eslint-disable-next-line no-var
  var __aero_io_port_write: ((port: number, size: number, value: number) => void) | undefined;
  // eslint-disable-next-line no-var
  var __aero_mmio_read: ((addr: bigint, size: number) => number) | undefined;
  // eslint-disable-next-line no-var
  var __aero_mmio_write: ((addr: bigint, size: number, value: number) => void) | undefined;
}

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
      code_version_table_configured: boolean;
      // i64/BigInt ABI smoke info (observed via `globalThis.__aero_jit_call`).
      jit_return_type: string | null;
      jit_return_is_sentinel: boolean;
      stale_install_rejected: boolean;
      stale_recompile_requested: boolean;
      stale_existing_slot_preserved: boolean;
      // Debug: number of unique guest physical pages whose code versions were bumped.
      // This is additive-only and may be absent in older builds.
      code_version_pages_bumped_total?: number;
    }
  | { type: 'CpuWorkerError'; reason: string };

type CpuWorkerStartMessage = {
  type: 'CpuWorkerStart';
  iterations?: number;
  threshold?: number;
};

const ENTRY_RIP = 0x1000;
const STALE_RIP = 0x2000;
const STALE_RACE_RIP = 0x3000;
const DEFAULT_MAX_BYTES = 1024;
// Debug-only sync word used by the JIT smoke test to coordinate a deterministic
// stale-code scenario.
//
// IMPORTANT: Do not use a low linear-memory address for this (it may overlap wasm
// statics/stack/heap). Instead we use a word inside the wasm runtime allocator's
// reserved tail guard (see `crates/aero-wasm/src/runtime_alloc.rs`).
//
// Keep this in sync with `HEAP_TAIL_GUARD_BYTES` (currently 64).
const DEBUG_SYNC_TAIL_GUARD_BYTES = 64;

// Tier-1 JIT call status slot (mirrors `crates/aero-jit-x86/src/jit_ctx.rs` + `crates/aero-wasm/src/tiered_vm.rs`).
//
// Layout (relative to `cpuPtr`):
//   CpuState (cpu_state_size bytes, sourced from `api.jit_abi_constants()`)
//   JitContext (header + inline TLB)
//   Tier-2 ctx
//   commit_flag (u32)
//
// JS sets `commit_flag = 0` when it rolls back architectural + memory effects on runtime exits.
// The tiered dispatcher uses this to avoid retiring guest instructions for rolled-back blocks.
// NOTE: These layout constants are sourced from the WASM build via `jit_abi_constants()` and
// `tiered_vm_jit_abi_layout()` so the JS worker cannot drift from the Rust-side memory layout
// contract.

// Tier-1 "exit to interpreter" sentinel return value (`u64::MAX` encoded as `i64`).
const JIT_EXIT_SENTINEL_I64 = -1n;

type HostExitState = { mmio_exit: boolean; jit_exit: boolean; page_fault: boolean };
type WriteLogEntry = { addr: number; size: number; paddr: bigint; old_value_bytes: Uint8Array };

function hostExitStateShouldRollback(state: HostExitState): boolean {
  return state.mmio_exit || state.jit_exit || state.page_fault;
}

function postToMain(msg: CpuWorkerToMainMessage) {
  ctx.postMessage(msg);
}

function formatDebugScalar(value: unknown): string {
  try {
    if (value === null) return 'null';
    if (value === undefined) return 'undefined';
    switch (typeof value) {
      case 'string':
        return value.length > 256 ? `${value.slice(0, 256)}…` : value;
      case 'number':
        return Number.isFinite(value) ? String(value) : 'NaN';
      case 'bigint':
        return value.toString();
      case 'boolean':
        return value ? 'true' : 'false';
      case 'symbol':
        return 'symbol';
      case 'function':
        return 'function';
      case 'object':
        return 'object';
      default:
        return 'unknown';
    }
  } catch {
    return '[unprintable]';
  }
}

function formatDebugObject(obj: Record<string, unknown>): string {
  try {
    const parts: string[] = [];
    for (const key of Object.keys(obj)) {
      let value: unknown;
      try {
        value = obj[key];
      } catch {
        value = '[threw]';
      }
      parts.push(`${key}=${formatDebugScalar(value)}`);
    }
    return `{ ${parts.join(' ')} }`;
  } catch {
    return '[unserializable]';
  }
}

type PendingCompile = {
  pre_meta: JsCompiledBlockMeta;
  resolve: (msg: CompileBlockResponse) => void;
  reject: (reason: string) => void;
};

let nextCompileId = 1;
const pendingCompiles = new Map<number, PendingCompile>();

const WASM_PAGE_BYTES = 64 * 1024;
const RUNTIME_RESERVED_BYTES = 128 * 1024 * 1024;
// Keep the guest RAM small: this worker is a Tier-1 JIT smoke harness and only needs enough
// space for a handful of tiny code/data buffers. The wasm runtime still reserves a fixed
// 128MiB region for its heap, so lowering guest RAM meaningfully reduces total
// SharedArrayBuffer/WebAssembly.Memory pressure in CI.
const DEFAULT_GUEST_RAM_BYTES = 1 * 1024 * 1024;
const X86_PAGE_BYTES = 4096;

// PC/Q35 guest physical layout constants.
//
// When the configured guest RAM exceeds LOW_RAM_END, the range [LOW_RAM_END..HIGH_RAM_START) is a
// PCI/ECAM/MMIO hole, and the remaining RAM bytes are remapped above 4GiB starting at HIGH_RAM_START.
//
// Keep these in sync with:
// - `crates/aero-wasm/src/guest_phys.rs`
// - `web/src/arch/guest_phys.ts` / `web/src/arch/guest_ram_translate.ts`
const LOW_RAM_END = BigInt(LOW_RAM_END_U32);
const HIGH_RAM_START = BigInt(HIGH_RAM_START_U53);

// Page-version snapshot metadata (`CompiledBlockMeta.page_versions`) serialized through wasm-bindgen.
//
// `version` is a modulo-2^32 u32 counter (wrapping on overflow) and is validated via equality
// checks against the current runtime/JIT page-version table.
type JsPageVersionSnapshot = { page: number; version: number };
type JsCompiledBlockMeta = { code_paddr: number; byte_len: number; page_versions: JsPageVersionSnapshot[] };

function shrinkMeta(pre: JsCompiledBlockMeta, codeByteLen: number): JsCompiledBlockMeta {
  const byteLen = Math.max(0, codeByteLen | 0) >>> 0;
  if (byteLen === 0) {
    return { code_paddr: pre.code_paddr, byte_len: 0, page_versions: [] };
  }

  const codePaddr = pre.code_paddr;
  const startPage = Math.floor(codePaddr / X86_PAGE_BYTES);
  const endPage = Math.floor((codePaddr + byteLen - 1) / X86_PAGE_BYTES);
  const page_versions = pre.page_versions.filter((snap) => snap.page >= startPage && snap.page <= endPage);
  return { code_paddr: codePaddr, byte_len: byteLen, page_versions };
}

function platformSharedMemoryError(err: unknown): string {
  const detail = formatOneLineError(err, 512);
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
      const n = Number(value);
      return Number.isFinite(n) ? n : 0;
    }

    return 0;
  } catch {
    return 0;
  }
}

function readMaybeU32(vm: unknown, key: string): number | undefined {
  if (!vm || (typeof vm !== 'object' && typeof vm !== 'function')) return undefined;
  let value: unknown;
  try {
    value = (vm as Record<string, unknown>)[key];
  } catch {
    return undefined;
  }
  if (value === undefined) return undefined;

  // wasm-bindgen may represent getters as methods in some builds.
  if (typeof value === 'function') {
    try {
      value = (value as (...args: never[]) => unknown).call(vm);
    } catch {
      return undefined;
    }
  }

  let n: number;
  if (typeof value === 'number') {
    n = value;
  } else if (typeof value === 'bigint') {
    if (value < 0n || value > 0xffff_ffffn) return undefined;
    n = Number(value);
  } else if (typeof value === 'string') {
    const trimmed = value.trim().toLowerCase();
    if (!trimmed) return undefined;
    const parsed = trimmed.startsWith('0x') ? Number.parseInt(trimmed.slice(2), 16) : Number.parseInt(trimmed, 10);
    if (!Number.isFinite(parsed)) return undefined;
    n = parsed;
  } else {
    return undefined;
  }

  if (!Number.isFinite(n) || n < 0 || n > 0xffff_ffff || !Number.isInteger(n)) return undefined;
  return n >>> 0;
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

  // Install safe default shims for the wasm32 VM loop.
  //
  // The Tiered VM's bus will call out to these globals on port I/O and MMIO exits. The JIT smoke
  // harness typically does not exercise those paths, but defining them here avoids surprising
  // `globalThis.__aero_*` missing traps if the guest code or runtime behavior changes.
  //
  // IMPORTANT: wasm `i64` values are represented as JS `bigint`, so `__aero_mmio_*` must use
  // `bigint` for the address parameter.
  const aeroPortShims = globalThis as unknown as {
    __aero_io_port_read?: (port: number, size: number) => number;
    __aero_io_port_write?: (port: number, size: number, value: number) => void;
    __aero_mmio_read?: (addr: bigint, size: number) => number;
    __aero_mmio_write?: (addr: bigint, size: number, value: number) => void;
  };
  if (typeof aeroPortShims.__aero_io_port_read !== 'function') {
    aeroPortShims.__aero_io_port_read = (_port: number, _size: number) => 0;
  }
  if (typeof aeroPortShims.__aero_io_port_write !== 'function') {
    aeroPortShims.__aero_io_port_write = (_port: number, _size: number, _value: number) => {};
  }
  if (typeof aeroPortShims.__aero_mmio_read !== 'function') {
    aeroPortShims.__aero_mmio_read = (_addr: bigint, _size: number) => 0;
  }
  if (typeof aeroPortShims.__aero_mmio_write !== 'function') {
    aeroPortShims.__aero_mmio_write = (_addr: bigint, _size: number, _value: number) => {};
  }

  let api: WasmApi;
  let variant: string;
  try {
    ({ api, variant } = await initWasmForContext({ variant: 'threaded', memory }));
  } catch (err) {
    postToMain({
      type: 'CpuWorkerError',
      reason: formatOneLineError(err, 512),
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
  const cpu_state_size = readMaybeU32(jitAbi, 'cpu_state_size');
  const cpu_state_align = readMaybeU32(jitAbi, 'cpu_state_align');
  const cpu_rip_off = readMaybeU32(jitAbi, 'cpu_rip_off');
  const cpu_rflags_off = readMaybeU32(jitAbi, 'cpu_rflags_off');
  const cpu_gpr_off_raw = (jitAbi as { cpu_gpr_off?: unknown } | null | undefined)?.cpu_gpr_off;
  if (
    cpu_state_size === undefined ||
    cpu_state_size === 0 ||
    cpu_state_align === undefined ||
    cpu_state_align === 0 ||
    cpu_rip_off === undefined ||
    cpu_rflags_off === undefined ||
    !(cpu_gpr_off_raw instanceof Uint32Array) ||
    cpu_gpr_off_raw.length !== 16
  ) {
    const cpu_gpr_off_ctor = cpu_gpr_off_raw && typeof cpu_gpr_off_raw === 'object' ? (cpu_gpr_off_raw as { constructor?: unknown }).constructor : null;
    const cpu_gpr_off_type = typeof cpu_gpr_off_ctor === 'function' ? cpu_gpr_off_ctor.name : typeof cpu_gpr_off_raw;
    const cpu_gpr_off_len =
      cpu_gpr_off_raw && typeof cpu_gpr_off_raw === 'object' && typeof (cpu_gpr_off_raw as { length?: unknown }).length === 'number'
        ? (cpu_gpr_off_raw as { length: number }).length
        : undefined;
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_abi_constants payload from aero-wasm: ${formatDebugObject({
        cpu_state_size,
        cpu_state_align,
        cpu_rip_off,
        cpu_rflags_off,
        cpu_gpr_off_type,
        cpu_gpr_off_len,
      })}`,
    });
    return;
  }
  const cpu_gpr_off = cpu_gpr_off_raw;

  if ((cpu_state_align & (cpu_state_align - 1)) !== 0 || cpu_state_size % cpu_state_align !== 0) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_abi_constants CpuState size/alignment: ${formatDebugObject({
        cpu_state_size,
        cpu_state_align,
      })}`,
    });
    return;
  }

  const cpu_rax_off = cpu_gpr_off[0]! >>> 0;
  if (cpu_rip_off + 8 > cpu_state_size || cpu_rflags_off + 8 > cpu_state_size || cpu_rax_off + 8 > cpu_state_size) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_abi_constants offsets (out of bounds): ${formatDebugObject({
        cpu_state_size,
        cpu_rax_off,
        cpu_rip_off,
        cpu_rflags_off,
      })}`,
    });
    return;
  }

  // Additional invariants: these offsets should always be 8-byte aligned since they are u64 fields.
  if ((cpu_rip_off & 7) !== 0 || (cpu_rflags_off & 7) !== 0 || (cpu_rax_off & 7) !== 0) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_abi_constants offsets (misaligned u64 fields): ${formatDebugObject({
        cpu_state_size,
        cpu_rax_off,
        cpu_rip_off,
        cpu_rflags_off,
      })}`,
    });
    return;
  }

  // The `CpuState` GPRs are stored as `[u64; 16]`, so offsets must be contiguous in 8-byte steps.
  for (let i = 0; i < 16; i++) {
    const expected = (cpu_rax_off + i * 8) >>> 0;
    const got = cpu_gpr_off[i]! >>> 0;
    if (got !== expected) {
      postToMain({
        type: 'CpuWorkerError',
        reason: `Invalid jit_abi_constants cpu_gpr_off array (unexpected layout): ${formatDebugObject({
          i,
          got,
          expected,
          cpu_rax_off,
        })}`,
      });
      return;
    }
  }
  if (cpu_rax_off + 16 * 8 > cpu_state_size) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_abi_constants cpu_gpr_off array (out of bounds): ${formatDebugObject({
        cpu_state_size,
        cpu_rax_off,
        cpu_r15_end: cpu_rax_off + 16 * 8,
      })}`,
    });
    return;
  }

  // Newer WASM builds expose Tier-1 JIT layout fields directly on `jit_abi_constants()` to avoid
  // JS-side drift. Fall back to the older `tiered_vm_jit_abi_layout()` helper for backwards
  // compatibility with deployed bundles.
  const jitCtxRamBaseOffset = (readMaybeU32(jitAbi, 'jit_ctx_ram_base_offset') ?? 0) >>> 0;
  const jitCtxTlbSaltOffset = (readMaybeU32(jitAbi, 'jit_ctx_tlb_salt_offset') ?? 8) >>> 0;
  const jitCtxTlbOffset = readMaybeU32(jitAbi, 'jit_ctx_tlb_offset');
  let jitCtxHeaderBytes = readMaybeU32(jitAbi, 'jit_ctx_header_bytes') ?? 0;
  let jitTlbEntries = readMaybeU32(jitAbi, 'jit_tlb_entries') ?? 0;
  let jitTlbEntryBytes = readMaybeU32(jitAbi, 'jit_tlb_entry_bytes') ?? 0;
  const jitTlbFlagRead = (readMaybeU32(jitAbi, 'jit_tlb_flag_read') ?? 1) >>> 0;
  const jitTlbFlagWrite = (readMaybeU32(jitAbi, 'jit_tlb_flag_write') ?? 2) >>> 0;
  const jitTlbFlagExec = (readMaybeU32(jitAbi, 'jit_tlb_flag_exec') ?? 4) >>> 0;
  const jitTlbFlagIsRam = (readMaybeU32(jitAbi, 'jit_tlb_flag_is_ram') ?? 8) >>> 0;
  const pageShift = (readMaybeU32(jitAbi, 'page_shift') ?? 12) >>> 0;
  const pageShiftBig = BigInt(pageShift);
  const pageSizeBig = 1n << pageShiftBig;
  let tier2CtxBytes = readMaybeU32(jitAbi, 'tier2_ctx_size') ?? 0;
  const tier2CtxOffset = readMaybeU32(jitAbi, 'tier2_ctx_offset');
  let commitFlagOffset = readMaybeU32(jitAbi, 'commit_flag_offset') ?? 0;
  const commitFlagBytes = readMaybeU32(jitAbi, 'commit_flag_bytes');

  if (!jitCtxHeaderBytes || !jitTlbEntries || !jitTlbEntryBytes || !tier2CtxBytes || !commitFlagOffset) {
    const abiLayoutFn = api.tiered_vm_jit_abi_layout;
    if (typeof abiLayoutFn !== 'function') {
      postToMain({
        type: 'CpuWorkerError',
        reason:
          'Missing Tier-1 JIT ABI layout helpers from aero-wasm (need either jit_abi_constants layout fields or tiered_vm_jit_abi_layout()). Rebuild the WASM package.',
      });
      return;
    }

    const abiLayout = abiLayoutFn();
    jitCtxHeaderBytes ||= readMaybeNumber(abiLayout, 'jit_ctx_header_bytes') >>> 0;
    jitTlbEntries ||= readMaybeNumber(abiLayout, 'jit_tlb_entries') >>> 0;
    jitTlbEntryBytes ||= readMaybeNumber(abiLayout, 'jit_tlb_entry_bytes') >>> 0;
    tier2CtxBytes ||= readMaybeNumber(abiLayout, 'tier2_ctx_bytes') >>> 0;
    commitFlagOffset ||= readMaybeNumber(abiLayout, 'commit_flag_offset') >>> 0;
  }

  if (!jitCtxHeaderBytes || !jitTlbEntries || !jitTlbEntryBytes || !tier2CtxBytes || !commitFlagOffset) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid Tier-1 JIT ABI layout values: ${formatDebugObject({
        jitCtxHeaderBytes,
        jitTlbEntries,
        jitTlbEntryBytes,
        tier2CtxBytes,
        commitFlagOffset,
      })}`,
    });
    return;
  }

  // Validate that the fixed header offsets exposed by `jit_abi_constants()` are usable with the
  // resolved JitContext header size. This helps catch Rust/JS ABI drift early.
  if (
    jitCtxHeaderBytes < 16 ||
    jitCtxRamBaseOffset + 8 > jitCtxHeaderBytes ||
    jitCtxTlbSaltOffset + 8 > jitCtxHeaderBytes ||
    (jitCtxRamBaseOffset & 7) !== 0 ||
    (jitCtxTlbSaltOffset & 7) !== 0
  ) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid JitContext header offsets: ${formatDebugObject({
        jitCtxHeaderBytes,
        jitCtxRamBaseOffset,
        jitCtxTlbSaltOffset,
      })}`,
    });
    return;
  }

  if (jitCtxTlbOffset !== undefined) {
    const off = jitCtxTlbOffset >>> 0;
    if (off !== jitCtxHeaderBytes || (off & 7) !== 0) {
      postToMain({
        type: 'CpuWorkerError',
        reason: `Invalid JitContext TLB offset: ${formatDebugObject({
          jitCtxHeaderBytes,
          jitCtxTlbOffset: off,
        })}`,
      });
      return;
    }
  }

  if ((jitCtxHeaderBytes & 7) !== 0) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_ctx_header_bytes (expected 8-byte alignment): ${formatDebugObject({
        jitCtxHeaderBytes,
      })}`,
    });
    return;
  }

  if (jitTlbEntryBytes < 16 || (jitTlbEntryBytes & 7) !== 0) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_tlb_entry_bytes (expected >=16 and 8-byte aligned): ${formatDebugObject({
        jitTlbEntryBytes,
      })}`,
    });
    return;
  }

  if ((jitTlbEntries & (jitTlbEntries - 1)) !== 0) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid jit_tlb_entries (expected power-of-two): ${formatDebugObject({
        jitTlbEntries,
      })}`,
    });
    return;
  }

  if (commitFlagBytes !== undefined && (commitFlagBytes >>> 0) !== 4) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid commit_flag_bytes (expected 4): ${formatDebugObject({
        commitFlagBytes: commitFlagBytes >>> 0,
      })}`,
    });
    return;
  }

  // Sanity checks: ensure the returned values are internally consistent (detect Rust/JS ABI drift).
  const derivedJitCtxTotalBytesBig = BigInt(jitCtxHeaderBytes) + BigInt(jitTlbEntries) * BigInt(jitTlbEntryBytes);
  if (derivedJitCtxTotalBytesBig > 0xffff_ffffn) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Tier-1 jit ctx size overflow (expected <= 4GiB): ${formatDebugObject({
        jitCtxHeaderBytes,
        jitTlbEntries,
        jitTlbEntryBytes,
      })}`,
    });
    return;
  }
  const derivedJitCtxTotalBytes = Number(derivedJitCtxTotalBytesBig) >>> 0;
  const exportedJitCtxTotalBytes = readMaybeU32(jitAbi, 'jit_ctx_total_bytes') ?? 0;
  if (exportedJitCtxTotalBytes !== 0 && exportedJitCtxTotalBytes !== derivedJitCtxTotalBytes) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Inconsistent jit_abi_constants payload: ${formatDebugObject({
        derivedJitCtxTotalBytes,
        exportedJitCtxTotalBytes,
        jitCtxHeaderBytes,
        jitTlbEntries,
        jitTlbEntryBytes,
      })}`,
    });
    return;
  }

  const expectedCommitFlagOffset = (cpu_state_size + derivedJitCtxTotalBytes + tier2CtxBytes) >>> 0;
  if (commitFlagOffset !== expectedCommitFlagOffset) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Inconsistent Tier-1 commit_flag_offset: ${formatDebugObject({
        commitFlagOffset,
        expectedCommitFlagOffset,
        cpu_state_size,
        derivedJitCtxTotalBytes,
        tier2CtxBytes,
      })}`,
    });
    return;
  }

  if ((commitFlagOffset & 3) !== 0) {
    postToMain({
      type: 'CpuWorkerError',
      reason: `Invalid commit_flag_offset alignment (expected 4-byte alignment): ${formatDebugObject({
        commitFlagOffset,
      })}`,
    });
    return;
  }

  const expectedTier2CtxOffset = (cpu_state_size + derivedJitCtxTotalBytes) >>> 0;
  if (tier2CtxOffset !== undefined) {
    const gotTier2CtxOffset = tier2CtxOffset >>> 0;
    if (gotTier2CtxOffset !== expectedTier2CtxOffset) {
      postToMain({
        type: 'CpuWorkerError',
        reason: `Inconsistent Tier-1 tier2_ctx_offset: ${formatDebugObject({
          gotTier2CtxOffset,
          expectedTier2CtxOffset,
          cpu_state_size,
          derivedJitCtxTotalBytes,
        })}`,
      });
      return;
    }
  }

  const resolvedTier2CtxOffset = (tier2CtxOffset !== undefined ? (tier2CtxOffset >>> 0) : expectedTier2CtxOffset) >>> 0;
  const abiCodeVersionTablePtrOffset = readMaybeU32(jitAbi, 'code_version_table_ptr_offset');
  const abiCodeVersionTableLenOffset = readMaybeU32(jitAbi, 'code_version_table_len_offset');
  const hasAbiCodeVersionOffsets =
    abiCodeVersionTablePtrOffset !== undefined &&
    abiCodeVersionTableLenOffset !== undefined &&
    (abiCodeVersionTablePtrOffset >>> 0) !== 0 &&
    (abiCodeVersionTableLenOffset >>> 0) !== 0;
  const candidateCodeVersionTablePtrOffset = hasAbiCodeVersionOffsets
    ? (abiCodeVersionTablePtrOffset! >>> 0)
    : ((resolvedTier2CtxOffset + 4) >>> 0);
  const candidateCodeVersionTableLenOffset = hasAbiCodeVersionOffsets
    ? (abiCodeVersionTableLenOffset! >>> 0)
    : ((resolvedTier2CtxOffset + 8) >>> 0);
  const codeVersionTableOffsetsValid =
    (candidateCodeVersionTablePtrOffset & 3) === 0 &&
    (candidateCodeVersionTableLenOffset & 3) === 0 &&
    candidateCodeVersionTablePtrOffset >= expectedTier2CtxOffset &&
    candidateCodeVersionTableLenOffset >= expectedTier2CtxOffset &&
    candidateCodeVersionTablePtrOffset + 4 <= commitFlagOffset &&
    candidateCodeVersionTableLenOffset + 4 <= commitFlagOffset;
  const codeVersionTablePtrOffset = codeVersionTableOffsetsValid ? candidateCodeVersionTablePtrOffset : null;
  const codeVersionTableLenOffset = codeVersionTableOffsetsValid ? candidateCodeVersionTableLenOffset : null;

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

  // Guest physical -> backing RAM translation (PC/Q35).
  //
  // Tier-1 blocks and their JS imports operate on *guest physical addresses*. When guest RAM grows
  // beyond `LOW_RAM_END`, the Q35 layout inserts an ECAM/PCI/MMIO hole below 4GiB and remaps the
  // remaining RAM above 4GiB. The wasm linear-memory backing store is still contiguous, so JS-side
  // helpers must translate physical addresses back into that backing store.
  const guest_size_u64 = BigInt(guest_size);
  const hasHighRamRemap = guest_size_u64 > LOW_RAM_END;

  const guestPaddrToRamOffset = (paddr: bigint): bigint | null => {
    const addr = asU64(paddr);
    if (!hasHighRamRemap) {
      return addr < guest_size_u64 ? addr : null;
    }

    if (addr < LOW_RAM_END) return addr;

    const highLen = guest_size_u64 - LOW_RAM_END;
    const highEnd = HIGH_RAM_START + highLen;
    if (addr >= HIGH_RAM_START && addr < highEnd) {
      return LOW_RAM_END + (addr - HIGH_RAM_START);
    }

    // Hole or out-of-range.
    return null;
  };

  const guestPaddrToLinear = (paddr: bigint, len: number): number | null => {
    const n = len >>> 0;
    if (n === 0) return guest_base;

    const baseOff = guestPaddrToRamOffset(paddr);
    if (baseOff === null) return null;

    if (n !== 1) {
      const lastPaddr = asU64(paddr) + BigInt(n - 1);
      const lastOff = guestPaddrToRamOffset(lastPaddr);
      if (lastOff === null) return null;
      // Ensure the translation is contiguous for the entire range (reject hole crossings).
      if (baseOff + BigInt(n - 1) !== lastOff) return null;
    }

    const offNum = Number(baseOff);
    if (!Number.isFinite(offNum) || offNum < 0 || offNum + n > guest_size) return null;
    return guest_base + offNum;
  };

  // Install JS-side tier-1 call table that the WASM tiered runtime imports via `globalThis.__aero_jit_call`.
  // Slots are recycled on cache eviction so the table stays bounded.
  const jitFns: Array<((cpu_ptr: number, jit_ctx_ptr: number) => bigint) | undefined> = [];
  let lastJitReturnType: string | null = null;
  let lastJitReturnIsSentinel = false;
  let codeVersionPagesBumpedTotal = 0;

  // Rollback state is scoped to a single `__aero_jit_call` invocation.
  // `env.*` imports consult these while a block is executing.
  let activeExitState: HostExitState | null = null;
  let activeWriteLog: WriteLogEntry[] | null = null;
  let onGuestWrite: ((paddr: bigint, len: number) => void) | null = null;

  // Shared views into the linear memory buffer. Note that `WebAssembly.Memory.grow` can swap out
  // `memory.buffer`, so these must be refreshed opportunistically.
  let dv = new DataView(memory.buffer);

  // Used for CPU state snapshots + write log byte copies.
  // NOTE: This is a view, not a copy; snapshotting uses `.slice()`.
  let memU8 = new Uint8Array(memory.buffer);
  const refreshMemU8 = () => {
    if (memU8.buffer === memory.buffer) return;
    dv = new DataView(memory.buffer);
    memU8 = new Uint8Array(memory.buffer);
  };

  // ---------------------------------------------------------------------------
  // Shared code-version table (CVT-WASM-001).
  // ---------------------------------------------------------------------------
  //
  // Newer WASM builds expose a dense `[u32]` page-version table in linear memory. The tiered VM
  // publishes it via two `u32` fields (relative to `cpuPtr`) so JS can bump code page versions
  // without calling back into WASM for every committed store.
  //
  // Entries are treated as modulo-2^32 counters (`u32::MAX + 1 == 0`). When the wasm memory is a
  // `SharedArrayBuffer`, bumps/reads use `Atomics.*` (which also wraps) for correctness under
  // concurrency.
  //
  // This worker caches the ptr/len + a `Uint32Array` view when possible. If the table is
  // unavailable (ptr/len == 0), we fall back to the legacy `vm.on_guest_write` callback when
  // available.
  //
  // Note: The Tiered VM sizes the table to cover the full *guest physical* span, which may include
  // the Q35 hole below 4GiB when `guest_size > LOW_RAM_END`. Validate the reported `(ptr,len)`
  // defensively to avoid accidentally treating uninitialized scratch `cpuPtr` buffers (used by
  // some ABI smoke tests) as a valid table header.
  let cachedCodeVersionCpuPtr: number | null = null;
  let codeVersionTablePtr = 0;
  let codeVersionTableLen = 0;
  let codeVersionTableU32: Uint32Array | null = null;
  let codeVersionTableU32Buffer: ArrayBufferLike | null = null;
  const expectedCodeVersionTableLen = (() => {
    const guestPhysEndExclusive =
      guest_size_u64 > LOW_RAM_END ? HIGH_RAM_START + (guest_size_u64 - LOW_RAM_END) : guest_size_u64;
    const pagesBig = (guestPhysEndExclusive + pageSizeBig - 1n) >> pageShiftBig;
    const pagesNum = Number(pagesBig);
    if (!Number.isFinite(pagesNum) || pagesNum <= 0) return 0;
    // The wasm ABI stores lengths as `u32`.
    return pagesNum > 0xffff_ffff ? 0xffff_ffff : (pagesNum >>> 0);
  })();

  const codeVersionTableUseAtomics = (): boolean => {
    // `Atomics` is required for SharedArrayBuffer; check defensively for older browsers/tests.
    return (
      typeof Atomics !== 'undefined' &&
      typeof SharedArrayBuffer !== 'undefined' &&
      memory.buffer instanceof SharedArrayBuffer
    );
  };

  const refreshCodeVersionTableView = (): void => {
    if (!codeVersionTablePtr || !codeVersionTableLen) {
      codeVersionTableU32 = null;
      codeVersionTableU32Buffer = null;
      return;
    }

    if (
      codeVersionTableU32 &&
      codeVersionTableU32Buffer === memory.buffer &&
      codeVersionTableU32.byteOffset === codeVersionTablePtr &&
      codeVersionTableU32.length === codeVersionTableLen
    ) {
      return;
    }

    if ((codeVersionTablePtr & 3) !== 0) {
      codeVersionTableU32 = null;
      codeVersionTableU32Buffer = null;
      return;
    }

    const endByte = BigInt(codeVersionTablePtr) + BigInt(codeVersionTableLen) * 4n;
    if (endByte > BigInt(memory.buffer.byteLength)) {
      codeVersionTableU32 = null;
      codeVersionTableU32Buffer = null;
      return;
    }

    try {
      codeVersionTableU32 = new Uint32Array(memory.buffer, codeVersionTablePtr, codeVersionTableLen);
      codeVersionTableU32Buffer = memory.buffer;
    } catch {
      codeVersionTableU32 = null;
      codeVersionTableU32Buffer = null;
    }
  };

  const refreshCodeVersionTableFromCpu = (cpuPtr: number): void => {
    if (codeVersionTablePtrOffset === null || codeVersionTableLenOffset === null) return;

    refreshMemU8();

    const cpuPtrU32 = cpuPtr >>> 0;
    // The tiered VM allocates its JIT ABI buffer (CpuState + JitContext + Tier-2 ctx) on the Rust
    // heap, which is bounded to the runtime-reserved region `[0, guest_base)`. Ignore refresh
    // requests for scratch `cpuPtr` values inside guest RAM (used by some ABI smoke tests).
    //
    // This also prevents accidental adoption of uninitialized `(ptr,len)` values from guest memory.
    if (cpuPtrU32 >= guest_base) return;
    let ptr = 0;
    let len = 0;
    try {
      ptr = dv.getUint32(cpuPtrU32 + codeVersionTablePtrOffset, true) >>> 0;
      len = dv.getUint32(cpuPtrU32 + codeVersionTableLenOffset, true) >>> 0;
    } catch {
      return;
    }

    const candidateValid = (() => {
      if (!ptr || !len) return false;
      if ((ptr & 3) !== 0) return false;
      const endByte = BigInt(ptr) + BigInt(len) * 4n;
      // The code-version table is allocated on the Rust heap which is bounded to the runtime
      // reserved region `[0, guest_base)`. Reject any pointers that overlap guest RAM.
      if (endByte > BigInt(guest_base)) return false;
      // Require the table to be at least large enough to cover the guest physical span; older
      // builds may choose a larger fixed size (e.g. 4GiB worth of pages).
      if (expectedCodeVersionTableLen && (len >>> 0) < expectedCodeVersionTableLen) return false;
      return endByte <= BigInt(memory.buffer.byteLength);
    })();

    if (!candidateValid) {
      // Only clear the cached table if the CPU pointer that used to own it is reporting it as
      // invalid. Ignore invalid headers from synthetic `__aero_jit_call` invocations that use a
      // scratch cpuPtr (BigInt ABI tests, rollback tests, etc).
      if (cachedCodeVersionCpuPtr === cpuPtrU32) {
        codeVersionTablePtr = 0;
        codeVersionTableLen = 0;
        codeVersionTableU32 = null;
        codeVersionTableU32Buffer = null;
      }
      return;
    }

    cachedCodeVersionCpuPtr = cpuPtrU32;
    if (codeVersionTablePtr !== ptr || codeVersionTableLen !== len) {
      codeVersionTablePtr = ptr;
      codeVersionTableLen = len;
      // Force recreation.
      codeVersionTableU32Buffer = null;
    }

    refreshCodeVersionTableView();
  };

  const ensureCodeVersionTableView = (): Uint32Array | null => {
    refreshMemU8();

    // If `memory.grow()` swapped the backing store, refresh the view.
    if (codeVersionTableU32 && codeVersionTableU32Buffer !== memory.buffer) {
      codeVersionTableU32Buffer = null;
    }
    if (!codeVersionTableU32 || codeVersionTableU32Buffer === null) {
      refreshCodeVersionTableView();
    }

    return codeVersionTableU32;
  };

  const readCodePageVersion = (page: bigint): bigint => {
    const table = ensureCodeVersionTableView();
    if (!table) return 0n;
    const idxBig = asU64(page);
    const len = codeVersionTableLen >>> 0;
    if (idxBig >= BigInt(len)) return 0n;
    const idx = Number(idxBig);
    const raw = codeVersionTableUseAtomics() ? Atomics.load(table, idx) : table[idx]!;
    return BigInt(raw >>> 0);
  };

  const bumpCodeVersions = (paddr: bigint, lenBytes: number): void => {
    const table = ensureCodeVersionTableView();
    if (!table) return;
    const n = lenBytes >>> 0;
    if (n === 0) return;

    const start = asU64(paddr);
    const u64MaxPlusOne = 0x1_0000_0000_0000_0000n;
    let endExclusive = start + BigInt(n);
    if (endExclusive > u64MaxPlusOne) endExclusive = u64MaxPlusOne;
    const endInclusive = endExclusive - 1n;

    const startPage = start >> pageShiftBig;
    const endPage = endInclusive >> pageShiftBig;
    const tableLenBig = BigInt(codeVersionTableLen >>> 0);
    if (tableLenBig === 0n || startPage >= tableLenBig) return;
    const clampedEndPage = endPage >= tableLenBig ? tableLenBig - 1n : endPage;

    const startIdx = Number(startPage);
    const endIdx = Number(clampedEndPage);
    if (startIdx > endIdx) return;

    if (codeVersionTableUseAtomics()) {
      for (let i = startIdx; i <= endIdx; i++) Atomics.add(table, i, 1);
    } else {
      for (let i = startIdx; i <= endIdx; i++) table[i] = (table[i]! + 1) >>> 0;
    }
  };

  const bumpOrNotifyGuestWrite = (paddr: bigint, lenBytes: number): void => {
    if (ensureCodeVersionTableView()) {
      bumpCodeVersions(paddr, lenBytes);
      return;
    }
    if (onGuestWrite) onGuestWrite(paddr, lenBytes);
  };

  globalThis.__aero_jit_call = (
    tableIndex: number,
    cpuPtr: number,
    jitCtxPtr: number,
  ): bigint => {
    const fn = jitFns[tableIndex];
    if (typeof fn !== 'function') {
      throw new Error(`missing JIT table entry ${tableIndex}`);
    }

    refreshMemU8();
    refreshCodeVersionTableFromCpu(cpuPtr);

    const commitFlagAddr = (cpuPtr + commitFlagOffset) >>> 0;
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
    const runtimeExit = hostExitStateShouldRollback(exitState);
    if (runtimeExit && !exitToInterpreter) {
      throw new Error(
        `Tier-1 JIT block triggered a runtime exit but returned a non-sentinel i64 (expected -1n). ret=${ret.toString()}`,
      );
    }
    const shouldRollback = exitToInterpreter && runtimeExit;

    if (shouldRollback) {
      // Roll back guest RAM writes (reverse order) and restore pre-block CPU state.
      refreshMemU8();
      for (let i = writeLog.length - 1; i >= 0; i--) {
        const entry = writeLog[i]!;
        memU8.set(entry.old_value_bytes, entry.addr);
      }
      memU8.set(cpuSnapshot, cpuPtr);
      dv.setUint32(commitFlagAddr, 0, true);
    } else if (writeLog.length) {
      // Notify the tiered runtime of committed guest writes so it can bump code page versions for
      // self-modifying code invalidation. We intentionally skip this on rolled-back exits.
      //
      // Realistic blocks can perform many stores to the same 4KiB page; de-duplicate version bumps
      // so we don't do O(N writes) Atomics/add overhead or inflate page-version counters.
      const touchedPages = new Set<number>();
      for (const entry of writeLog) {
        const size = entry.size >>> 0;
        if (size === 0) continue;
        const start = asU64(entry.paddr);
        const u64MaxPlusOne = 0x1_0000_0000_0000_0000n;
        let endExclusive = start + BigInt(size);
        if (endExclusive > u64MaxPlusOne) endExclusive = u64MaxPlusOne;
        const endInclusive = endExclusive - 1n;

        const startPage = start >> pageShiftBig;
        const endPage = endInclusive >> pageShiftBig;
        const startPageNum = Number(startPage);
        const endPageNum = Number(endPage);
        if (
          !Number.isSafeInteger(startPageNum) ||
          !Number.isSafeInteger(endPageNum) ||
          startPageNum < 0 ||
          endPageNum < startPageNum
        ) {
          continue;
        }
        for (let page = startPageNum; page <= endPageNum; page++) {
          touchedPages.add(page);
        }
      }

      const table = ensureCodeVersionTableView();
      if (table) {
        const len = codeVersionTableLen >>> 0;
        const useAtomics = codeVersionTableUseAtomics();
        for (const page of touchedPages) {
          if (page < 0 || page >= len) continue;
          if (useAtomics) {
            Atomics.add(table, page, 1);
          } else {
            table[page] = (table[page]! + 1) >>> 0;
          }
          codeVersionPagesBumpedTotal++;
        }
      } else if (onGuestWrite) {
        for (const page of touchedPages) {
          if (page < 0) continue;
          onGuestWrite(BigInt(page) << pageShiftBig, 1);
          codeVersionPagesBumpedTotal++;
        }
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
  const handleJitMessage = (ev: MessageEvent<JitToCpuMessage>) => {
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
  };
  jitWorker.addEventListener('message', handleJitMessage);

  const initMsg: CpuToJitMessage = { type: 'JitWorkerInit', memory, guest_base, guest_size };
  jitWorker.postMessage(initMsg);

  function startCompileOn(worker: Worker, entry_rip: number, opts: { max_bytes: number; debug_sync?: boolean }) {
    const id = nextCompileId++;
    const maxBytes = opts.max_bytes > 0 ? opts.max_bytes : DEFAULT_MAX_BYTES;
    const snapshotLen = maxBytes + 15;

    const pre_meta = vm.snapshot_meta(BigInt(entry_rip), snapshotLen) as unknown as JsCompiledBlockMeta;

    const req: CpuToJitMessage = {
      type: 'CompileBlockRequest',
      id,
      entry_rip,
      mode: 'tier1',
      max_bytes: maxBytes,
      // The smoke harness runs the guest in 16-bit real mode.
      bitness: 16,
      ...(opts.debug_sync ? { debug_sync: true } : {}),
    };

    worker.postMessage(req);

    const response = new Promise<CompileBlockResponse>((resolve, reject) => {
      pendingCompiles.set(id, {
        pre_meta,
        resolve,
        reject: (reason) => reject(new Error(reason)),
      });
    });

    return { id, pre_meta, response };
  }

  function startCompile(entry_rip: number, opts: { max_bytes: number; debug_sync?: boolean }) {
    return startCompileOn(jitWorker, entry_rip, opts);
  }

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

  const logWrite = (linearAddr: number, size: number, paddr: bigint) => {
    const log = activeWriteLog;
    if (!log) return;
    refreshMemU8();
    log.push({
      addr: linearAddr,
      size,
      paddr,
      old_value_bytes: memU8.slice(linearAddr, linearAddr + size),
    });
  };

  const readGuestU8 = (paddr: bigint): number => {
    refreshMemU8();
    const linear = guestPaddrToLinear(paddr, 1);
    if (linear === null) return 0;
    return dv.getUint8(linear);
  };

  const readGuestU16 = (paddr: bigint): number => {
    refreshMemU8();
    const linear = guestPaddrToLinear(paddr, 2);
    if (linear !== null) return dv.getUint16(linear, true);
    return readGuestU8(paddr) | (readGuestU8(asU64(paddr) + 1n) << 8);
  };

  const readGuestU32 = (paddr: bigint): number => {
    refreshMemU8();
    const linear = guestPaddrToLinear(paddr, 4);
    if (linear !== null) return dv.getUint32(linear, true) >>> 0;
    return (
      readGuestU8(paddr) |
      (readGuestU8(asU64(paddr) + 1n) << 8) |
      (readGuestU8(asU64(paddr) + 2n) << 16) |
      (readGuestU8(asU64(paddr) + 3n) << 24)
    ) >>> 0;
  };

  const readGuestU64 = (paddr: bigint): bigint => {
    refreshMemU8();
    const linear = guestPaddrToLinear(paddr, 8);
    if (linear !== null) return dv.getBigUint64(linear, true);
    let out = 0n;
    for (let i = 0n; i < 8n; i++) {
      out |= BigInt(readGuestU8(asU64(paddr) + i)) << (i * 8n);
    }
    return out;
  };

  const writeGuestU8 = (paddr: bigint, value: number): void => {
    refreshMemU8();
    const paddrU64 = asU64(paddr);
    const linear = guestPaddrToLinear(paddrU64, 1);
    if (linear === null) return;
    logWrite(linear, 1, paddrU64);
    dv.setUint8(linear, value & 0xff);
    // If the helper is used outside a JIT block (unlikely), still bump code versions.
    if (!activeWriteLog) bumpOrNotifyGuestWrite(paddrU64, 1);
  };

  // Tier-1 imports required by generated blocks (even if the smoke block doesn't use them).
  const env = {
    memory,
    mem_read_u8: (_cpuPtr: number, addr: bigint) => readGuestU8(addr),
    mem_read_u16: (_cpuPtr: number, addr: bigint) => readGuestU16(addr),
    mem_read_u32: (_cpuPtr: number, addr: bigint) => readGuestU32(addr) | 0,
    mem_read_u64: (_cpuPtr: number, addr: bigint) => asI64(readGuestU64(addr)),
    // Tier-2 codegen may optionally import this for code-version guards. It returns a u32 encoded as i64.
    code_page_version: (cpuPtr: number, page: bigint) => {
      refreshCodeVersionTableFromCpu(cpuPtr);
      return readCodePageVersion(page);
    },
    mem_write_u8: (_cpuPtr: number, addr: bigint, value: number) => {
      writeGuestU8(addr, value);
    },
    mem_write_u16: (_cpuPtr: number, addr: bigint, value: number) => {
      refreshMemU8();
      const paddrU64 = asU64(addr);
      const linear = guestPaddrToLinear(paddrU64, 2);
      if (linear !== null) {
        logWrite(linear, 2, paddrU64);
        dv.setUint16(linear, value & 0xffff, true);
        if (!activeWriteLog) bumpOrNotifyGuestWrite(paddrU64, 2);
        return;
      }
      writeGuestU8(paddrU64, value);
      writeGuestU8(paddrU64 + 1n, value >>> 8);
    },
    mem_write_u32: (_cpuPtr: number, addr: bigint, value: number) => {
      refreshMemU8();
      const paddrU64 = asU64(addr);
      const linear = guestPaddrToLinear(paddrU64, 4);
      if (linear !== null) {
        logWrite(linear, 4, paddrU64);
        dv.setUint32(linear, value >>> 0, true);
        if (!activeWriteLog) bumpOrNotifyGuestWrite(paddrU64, 4);
        return;
      }
      writeGuestU8(paddrU64, value);
      writeGuestU8(paddrU64 + 1n, value >>> 8);
      writeGuestU8(paddrU64 + 2n, value >>> 16);
      writeGuestU8(paddrU64 + 3n, value >>> 24);
    },
    mem_write_u64: (_cpuPtr: number, addr: bigint, value: bigint) => {
      refreshMemU8();
      const paddrU64 = asU64(addr);
      const linear = guestPaddrToLinear(paddrU64, 8);
      if (linear !== null) {
        logWrite(linear, 8, paddrU64);
        dv.setBigUint64(linear, asU64(value), true);
        if (!activeWriteLog) bumpOrNotifyGuestWrite(paddrU64, 8);
        return;
      }
      // Fallback: bytewise write (for hole straddles / oob).
      const v = asU64(value);
      for (let i = 0n; i < 8n; i++) {
        writeGuestU8(paddrU64 + i, Number((v >> (i * 8n)) & 0xffn));
      }
    },
    mmu_translate: (_cpuPtr: number, jitCtxPtr: number, vaddr: bigint, _access: number) => {
      const vaddrU = asU64(vaddr);
      const vpn = vaddrU >> pageShiftBig;
      const tlbEntriesBig = BigInt(jitTlbEntries);
      const tlbMaskBig = tlbEntriesBig - 1n;
      const idx = Number((tlbEntriesBig & tlbMaskBig) === 0n ? vpn & tlbMaskBig : vpn % tlbEntriesBig) >>> 0;

      const tlbSalt = dv.getBigUint64(jitCtxPtr + jitCtxTlbSaltOffset, true);
      const tag = asU64((vpn ^ tlbSalt) | 1n);

      const physBase = vaddrU & -pageSizeBig;
      const isRam = guestPaddrToLinear(physBase, Number(pageSizeBig)) !== null;
      const flags =
        BigInt(jitTlbFlagRead) |
        BigInt(jitTlbFlagWrite) |
        BigInt(jitTlbFlagExec) |
        (isRam ? BigInt(jitTlbFlagIsRam) : 0n);
      const data = asU64(physBase | flags);

      const entryAddr = jitCtxPtr + jitCtxHeaderBytes + idx * jitTlbEntryBytes;
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
      // Pick deterministic addresses in guest RAM that are not touched by the hot-loop code at 0x1000.
      // We use guest RAM as a scratch region for the Tier-1 ABI buffer (CpuState + jit_ctx + tier2_ctx + commit flag).
      const cpuPtr = (guest_base + 0xa000) >>> 0;
      const jitCtxPtr = (cpuPtr + cpu_state_size) >>> 0;
      if (cpuPtr + commitFlagOffset + 4 > guest_base + guest_size) return false;

      // Initialize the minimal JitContext header expected by our `mmu_translate` stub.
      dv.setBigUint64(jitCtxPtr + jitCtxRamBaseOffset, BigInt(guest_base), true); // ram_base
      dv.setBigUint64(jitCtxPtr + jitCtxTlbSaltOffset, 0n, true); // tlb_salt

      const run = async (bytes: Uint8Array<ArrayBuffer>): Promise<boolean> => {
        const module = await WebAssembly.compile(bytes);
        const instance = await WebAssembly.instantiate(module, { env });
        const block = (instance.exports as { block?: unknown }).block;
        if (typeof block !== 'function') return false;

        const tableIndex = nextTableIndex++;
        jitFns[tableIndex] = block as (cpu_ptr: number, jit_ctx_ptr: number) => bigint;
        const ret = globalThis.__aero_jit_call!(tableIndex, cpuPtr, jitCtxPtr);
        return typeof ret === 'bigint';
      };

      // Avoid notifying the tiered runtime of these synthetic guest writes; this is purely an ABI smoke check.
      const savedOnGuestWrite = onGuestWrite;
      onGuestWrite = null;
      try {
        if (!(await run(JIT_BIGINT_ABI_WASM_BYTES))) return false;
        if (!(await run(JIT_CODE_PAGE_VERSION_ABI_WASM_BYTES))) return false;
        return true;
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

        const ret = globalThis.__aero_jit_call!(tableIndex, cpuPtr, 0);
        if (ret !== JIT_EXIT_SENTINEL_I64) return false;

        refreshMemU8();
        const cpuAfter = memU8.slice(cpuPtr, cpuPtr + cpu_state_size);
        const storeAfter = dv.getUint32(storeLinear, true);
        const commitAfter = dv.getUint32(cpuPtr + commitFlagOffset, true);

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

      // Safety: if the block triggers a runtime exit helper but forgets to return the sentinel,
      // `__aero_jit_call` must not silently treat the block as a committed normal return.
      const badIndex = nextTableIndex++;
      jitFns[badIndex] = (cpu_ptr: number, _jit_ctx_ptr: number): bigint => {
        env.jit_exit(0, 0n);
        return 0n;
      };
      let badThrew = false;
      try {
        globalThis.__aero_jit_call!(badIndex, cpuPtr, 0);
      } catch {
        badThrew = true;
      }
      if (!badThrew) return false;

      // Separate check: seed the commit flag with 0 so we can confirm `__aero_jit_call` resets it
      // to 1 on entry for non-rollback paths.
      const committedIndex = nextTableIndex++;
      jitFns[committedIndex] = (cpu_ptr: number, _jit_ctx_ptr: number): bigint => {
        const rax = dv.getBigUint64(cpu_ptr + cpu_rax_off, true);
        dv.setBigUint64(cpu_ptr + cpu_rax_off, rax + 2n, true);
        return JIT_EXIT_SENTINEL_I64;
      };
      dv.setUint32(cpuPtr + commitFlagOffset, 0, true);
      dv.setBigUint64(cpuPtr + cpu_rax_off, preRax, true);
      const retCommitted = globalThis.__aero_jit_call!(committedIndex, cpuPtr, 0);
      if (retCommitted !== JIT_EXIT_SENTINEL_I64) return false;
      const commitCommitted = dv.getUint32(cpuPtr + commitFlagOffset, true);
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

  function pageSnapshotsEqual(a: JsPageVersionSnapshot[], b: JsPageVersionSnapshot[]): boolean {
    if (a.length !== b.length) return false;
    const byPage = new Map<number, number>();
    for (const snap of a) {
      byPage.set(snap.page, snap.version);
    }
    // Reject duplicates.
    if (byPage.size !== a.length) return false;
    for (const snap of b) {
      const version = byPage.get(snap.page);
      if (version === undefined) return false;
      if (version !== snap.version) return false;
    }
    return byPage.size === b.length;
  }

  function compiledMetaMatchesCurrent(entryRipU32: number, meta: JsCompiledBlockMeta): boolean {
    const current = vm.snapshot_meta(BigInt(entryRipU32), meta.byte_len) as unknown as JsCompiledBlockMeta;
    return (
      current.code_paddr === meta.code_paddr &&
      current.byte_len === meta.byte_len &&
      pageSnapshotsEqual(current.page_versions, meta.page_versions)
    );
  }

  async function installTier1(resp: CompileBlockResponse, pre_meta: JsCompiledBlockMeta): Promise<number | null> {
    const entryRipU32 = resp.entry_rip >>> 0;
    const meta = shrinkMeta(pre_meta, resp.meta.code_byte_len);

    const handleStaleMeta = (): number | null => {
      // Ask the runtime to process the stale handle so it can drop any stale existing block and/or
      // request a fresh compilation.
      try {
        // `tableIndex` is irrelevant because the handle will be rejected.
        vm.install_handle(BigInt(entryRipU32), 0, meta);
      } catch {
        // ignore
      }

      // If the runtime dropped the compiled block, keep our JS-side table mapping in sync.
      if (!vm.is_compiled(BigInt(entryRipU32))) {
        const idx = installedByRip.get(entryRipU32);
        if (idx !== undefined) {
          installedByRip.delete(entryRipU32);
          if (entryRipU32 === ENTRY_RIP && installedIndex === idx) {
            installedIndex = null;
          }
          freeTableIndex(idx);
        }
        return null;
      }

      return installedByRip.get(entryRipU32) ?? null;
    };

    // Safety: if we already have a valid compiled block for this RIP, the runtime will ignore a
    // stale background compilation result. In that case we must NOT overwrite the JS call-table
    // slot for the existing block with the stale compiled function.
    //
    // Detect staleness by comparing the pre-snapshotted page-version metadata (captured before the
    // JIT worker read the code bytes) against the current page-version snapshot.
    if (!compiledMetaMatchesCurrent(entryRipU32, meta)) {
      return handleStaleMeta();
    }

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

    // Re-check staleness after module compilation/instantiation. This protects against races where
    // another compilation installs a newer valid block while we were awaiting the WebAssembly
    // instantiation (e.g. when multiple JIT workers are running).
    if (!compiledMetaMatchesCurrent(entryRipU32, meta)) {
      return handleStaleMeta();
    }

    // Reuse the existing table slot if this RIP was compiled before. This makes recompilation
    // (self-modifying code invalidation) overwrite the previous slot rather than growing the JS
    // call table unboundedly.
    const existingIndex = installedByRip.get(entryRipU32);
    const tableIndex = existingIndex ?? allocTableIndex();
    jitFns[tableIndex] = block as (cpu_ptr: number, jit_ctx_ptr: number) => bigint;

    // wasm-bindgen APIs differ across versions. Capture as `unknown` so we can best-effort free
    // table indices without breaking typecheck if the return type changes (Array vs typed array vs
    // void in older builds).
    let evicted: unknown;
    try {
      evicted = vm.install_handle(BigInt(entryRipU32), tableIndex, meta) as unknown;
    } catch {
      // Backwards-compat: older wasm-bindgen exports may use u32 params (number) instead of u64 (BigInt).
      evicted = (vm.install_handle as unknown as (...args: unknown[]) => unknown)(entryRipU32, tableIndex, meta);
    }

    if (vm.is_compiled(BigInt(entryRipU32))) {
      installedByRip.set(entryRipU32, tableIndex);
    } else {
      // Installation was rejected (e.g. stale compilation result). Drop this slot so it can be reused.
      installedByRip.delete(entryRipU32);
      if (installedIndex === tableIndex) installedIndex = null;
      freeTableIndex(tableIndex);
    }

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
        } else if (typeof v === 'number' && Number.isFinite(v)) {
          releaseEvictedRip(v);
        }
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
    return installedByRip.get(entryRipU32) ?? null;
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
      const job = startCompile(entryRipU32, { max_bytes: DEFAULT_MAX_BYTES });
      const resp = await job.response;
      const idx = await installTier1(resp, job.pre_meta);
      if (entryRipU32 === ENTRY_RIP && idx !== null && vm.is_compiled(BigInt(entryRipU32))) {
        installedIndex = idx;
      }
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
        reason: `Tiered VM run_blocks failed: ${formatOneLineError(err, 512)}`,
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
        reason: `JIT compile failed: ${formatOneLineError(err, 512)}`,
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
    const jitTotal = Math.max(
      jit_executions,
      readMaybeNumber(vm, 'jit_blocks_total'),
      readMaybeNumber(vm, 'jit_executions'),
    );
    if (interpTotal > 0 && jitTotal > 0 && installedIndex !== null) {
      break;
    }

    // Yield so the JIT worker can run in parallel.
    await new Promise((r) => {
      const t = setTimeout(r, 0);
      unrefBestEffort(t);
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
      if (!ensureCodeVersionTableView() && !onGuestWrite) {
        throw new Error(
          'Cannot bump code page versions (missing shared code-version table and WasmTieredVm.on_guest_write/jit_on_guest_write)',
        );
      }
      bumpOrNotifyGuestWrite(BigInt(ENTRY_RIP), patched.byteLength);

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
          unrefBestEffort(t);
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
        reason: `Self-modifying code regression: unexpected error during recompilation: ${formatOneLineError(err, 512)}`,
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

  // ---------------------------------------------------------------------------
  // Forced stale compilation scenario.
  // ---------------------------------------------------------------------------
  let stale_install_rejected = false;
  let stale_recompile_requested = false;
  let stale_existing_slot_preserved = false;
  try {
    // Ensure no pending requests pollute the stale test.
    vm.drain_compile_requests();

    // Write a copy of the hot-loop at 0x2000.
    new Uint8Array(memory.buffer).set(code, guest_base + STALE_RIP);

    // Coordinate with the JIT worker so we can mutate guest bytes after it reads them.
    if (guest_base < DEBUG_SYNC_TAIL_GUARD_BYTES) {
      throw new Error(
        `debug_sync unavailable: guest_base (${guest_base}) < tail guard bytes (${DEBUG_SYNC_TAIL_GUARD_BYTES})`,
      );
    }
    const debugSyncOffset = guest_base - DEBUG_SYNC_TAIL_GUARD_BYTES;
    if ((debugSyncOffset & 3) !== 0 || debugSyncOffset + 4 > memory.buffer.byteLength) {
      throw new Error(`debug_sync offset out of bounds: offset=${debugSyncOffset} guest_base=${guest_base}`);
    }
    const sync = new Int32Array(memory.buffer, debugSyncOffset, 1);
    Atomics.store(sync, 0, 0);

    const staleJob = startCompile(STALE_RIP, { max_bytes: DEFAULT_MAX_BYTES, debug_sync: true });

    // Wait for the JIT worker to reach the barrier (sets the slot to `id`).
    Atomics.wait(sync, 0, 0, 5_000);
    const seen = Atomics.load(sync, 0);
    if (seen === staleJob.id) {
      // Mutate one byte of guest code and bump the page-version tracker.
      const u8 = new Uint8Array(memory.buffer);
      const immOff = STALE_RIP + 3;
      u8[guest_base + immOff] ^= 0x01;
      bumpOrNotifyGuestWrite(BigInt(immOff), 1);
    }

    // Release the JIT worker to respond.
    Atomics.store(sync, 0, -staleJob.id);
    Atomics.notify(sync, 0);

    const resp = await staleJob.response;
    await installTier1(resp, staleJob.pre_meta);

    stale_install_rejected = !vm.is_compiled(BigInt(STALE_RIP));
    const recompile = vm.drain_compile_requests();
    stale_recompile_requested = Array.from(recompile as unknown as Iterable<unknown>).some((rip) => {
      if (typeof rip === 'bigint') return u64AsNumber(rip) === STALE_RIP;
      if (typeof rip === 'number' && Number.isFinite(rip)) return (rip >>> 0) === STALE_RIP;
      return false;
    });
  } catch (err) {
    console.warn(`[cpu-worker] stale compile scenario failed: ${formatOneLineError(err, 512)}`);
  }

  // ---------------------------------------------------------------------------
  // Race scenario: a stale compilation result arriving after a newer valid block
  // must not clobber the JS call-table slot for the valid block.
  // ---------------------------------------------------------------------------
  let jitWorker2: Worker | null = null;
  try {
    // Ensure no pending requests pollute the race test.
    vm.drain_compile_requests();

    // Prepare a second JIT worker so we can keep one blocked on debug_sync while
    // the other compiles in parallel.
    jitWorker2 = new Worker(new URL('./jit-worker.ts', import.meta.url), { type: 'module' });
    jitWorker2.addEventListener('message', handleJitMessage);
    jitWorker2.postMessage(initMsg);

    // Seed guest code bytes at 0x3000.
    new Uint8Array(memory.buffer).set(code, guest_base + STALE_RACE_RIP);

    if (!ensureCodeVersionTableView() && !onGuestWrite) {
      throw new Error('Cannot bump code page versions (missing code-version table and on_guest_write); cannot run stale race test');
    }

    if (guest_base < DEBUG_SYNC_TAIL_GUARD_BYTES) {
      throw new Error(
        `debug_sync unavailable: guest_base (${guest_base}) < tail guard bytes (${DEBUG_SYNC_TAIL_GUARD_BYTES})`,
      );
    }
    const debugSyncOffset = guest_base - DEBUG_SYNC_TAIL_GUARD_BYTES;
    if ((debugSyncOffset & 3) !== 0 || debugSyncOffset + 4 > memory.buffer.byteLength) {
      throw new Error(`debug_sync offset out of bounds: offset=${debugSyncOffset} guest_base=${guest_base}`);
    }
    const sync = new Int32Array(memory.buffer, debugSyncOffset, 1);
    Atomics.store(sync, 0, 0);

    // Compile job A on worker 1 and block it at the debug_sync barrier.
    const jobA = startCompileOn(jitWorker, STALE_RACE_RIP, { max_bytes: DEFAULT_MAX_BYTES, debug_sync: true });
    Atomics.wait(sync, 0, 0, 5_000);
    const seen = Atomics.load(sync, 0);
    if (seen !== jobA.id) {
      throw new Error(`debug_sync barrier not reached for jobA (expected ${jobA.id}, got ${seen})`);
    }

    // Mutate guest bytes and bump page versions so jobA is stale.
    const u8 = new Uint8Array(memory.buffer);
    // Patch the hot-loop immediate (`add eax, imm8`) from 1 -> 0. Mutating the operand-size prefix
    // (0x66 -> 0x67) can cause Tier-1 compilation to bail out at entry in newer builds (unsupported
    // address-size override), so keep the first instruction shape intact.
    const patchOffset = 3; // imm8
    u8[guest_base + STALE_RACE_RIP + patchOffset] ^= 0x01;
    bumpOrNotifyGuestWrite(BigInt(STALE_RACE_RIP + patchOffset), 1);

    // Compile job B on worker 2 with a fresh meta snapshot.
    const jobB = startCompileOn(jitWorker2, STALE_RACE_RIP, { max_bytes: DEFAULT_MAX_BYTES });
    const respB = await jobB.response;
    await installTier1(respB, jobB.pre_meta);

    const idx = installedByRip.get(STALE_RACE_RIP);
    if (idx === undefined) {
      throw new Error('race test: expected STALE_RACE_RIP to be installed');
    }
    const fnBefore = jitFns[idx];

    // Release jobA so it can respond with a stale compilation result.
    Atomics.store(sync, 0, -jobA.id);
    Atomics.notify(sync, 0);

    const respA = await jobA.response;
    await installTier1(respA, jobA.pre_meta);

    const fnAfter = jitFns[idx];
    stale_existing_slot_preserved = typeof fnBefore === 'function' && fnBefore === fnAfter;

  } catch (err) {
    console.warn(`[cpu-worker] stale race scenario failed: ${formatOneLineError(err, 512)}`);
  } finally {
    jitWorker2?.terminate();
  }

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
    code_version_table_configured: ensureCodeVersionTableView() !== null,
    jit_return_type: lastJitReturnType,
    jit_return_is_sentinel: lastJitReturnIsSentinel,
    stale_install_rejected,
    stale_recompile_requested,
    stale_existing_slot_preserved,
    code_version_pages_bumped_total: codeVersionPagesBumpedTotal,
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
