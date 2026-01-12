import { expect, test } from '@playwright/test';

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? 'http://127.0.0.1:4173';
const ENTRY_RIP = 0x1000;

function getPath(obj: unknown, path: string): unknown {
  if (!obj || typeof obj !== 'object') return undefined;
  let cur: unknown = obj;
  for (const part of path.split('.')) {
    if (!cur || typeof cur !== 'object') return undefined;
    cur = (cur as Record<string, unknown>)[part];
  }
  return cur;
}

function parseMaybeNumber(value: unknown): number | undefined {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string') {
    const trimmed = value.trim().toLowerCase();
    if (!trimmed) return undefined;
    const parsed = trimmed.startsWith('0x') ? Number.parseInt(trimmed.slice(2), 16) : Number.parseInt(trimmed, 10);
    if (Number.isFinite(parsed)) return parsed;
  }
  return undefined;
}

function firstNumberAtPaths(obj: unknown, paths: string[]): number | undefined {
  for (const path of paths) {
    const v = parseMaybeNumber(getPath(obj, path));
    if (v !== undefined) return v;
  }
  return undefined;
}

function findBlockTableIndexByRip(result: unknown, rip: number): number | null {
  if (!result || typeof result !== 'object') return null;
  const r = result as Record<string, unknown>;

  // Legacy placeholder schema.
  const legacyInstalledRip = parseMaybeNumber(r.runtime_installed_entry_rip);
  const legacyTableIndex = parseMaybeNumber(r.runtime_installed_table_index);
  if (legacyInstalledRip === rip && legacyTableIndex !== undefined) return legacyTableIndex;

  // Newer schemas may report installed blocks as an array.
  const candidateArrays: unknown[] = [
    r.installed_blocks,
    r.installedBlocks,
    (r.blocks && typeof r.blocks === 'object' ? (r.blocks as Record<string, unknown>).installed_blocks : undefined),
    (r.blocks && typeof r.blocks === 'object' ? (r.blocks as Record<string, unknown>).installedBlocks : undefined),
    (r.blocks && typeof r.blocks === 'object' ? (r.blocks as Record<string, unknown>).installed : undefined),
    r.installed,
  ];
  for (const maybeArr of candidateArrays) {
    if (!Array.isArray(maybeArr)) continue;
    for (const entry of maybeArr) {
      if (!entry || typeof entry !== 'object') continue;
      const e = entry as Record<string, unknown>;
      const entryRip = parseMaybeNumber(e.entry_rip ?? e.entryRip ?? e.rip);
      if (entryRip !== rip) continue;
      const idx = parseMaybeNumber(e.table_index ?? e.tableIndex ?? e.table_idx ?? e.tableIdx ?? e.index ?? e.idx);
      if (idx !== undefined) return idx;
    }
  }

  // Mapping form: { [rip]: tableIndex } or { [rip]: { tableIndex } }.
  const candidateMaps: unknown[] = [
    r.installed_by_rip,
    r.installedByRip,
    r.installed_table_index_by_rip,
    r.installedTableIndexByRip,
  ];
  for (const maybeMap of candidateMaps) {
    if (!maybeMap || typeof maybeMap !== 'object' || Array.isArray(maybeMap)) continue;
    for (const [key, value] of Object.entries(maybeMap as Record<string, unknown>)) {
      const keyRip = parseMaybeNumber(key);
      if (keyRip !== rip) continue;
      const idx = parseMaybeNumber(
        typeof value === 'object' && value !== null
          ? (value as Record<string, unknown>).table_index ?? (value as Record<string, unknown>).tableIndex
          : value,
      );
      if (idx !== undefined) return idx;
    }
  }

  // Another legacy/placeholder convenience: direct installed_table_index.
  const fallbackIdx = parseMaybeNumber(r.installed_table_index);
  if (fallbackIdx !== undefined && legacyInstalledRip === rip) return fallbackIdx;

  return null;
}

function hasCompiledRip(result: unknown, rip: number): boolean {
  if (!result || typeof result !== 'object') return false;
  const r = result as Record<string, unknown>;

  // If we can prove install, we can safely treat that as evidence of compilation as well.
  if (findBlockTableIndexByRip(result, rip) !== null) return true;

  const candidateArrays: unknown[] = [
    r.compiled_blocks,
    r.compiledBlocks,
    r.compiles,
    r.compilations,
    r.compile_responses,
    r.compileResponses,
    (r.blocks && typeof r.blocks === 'object' ? (r.blocks as Record<string, unknown>).compiled_blocks : undefined),
    (r.blocks && typeof r.blocks === 'object' ? (r.blocks as Record<string, unknown>).compiledBlocks : undefined),
    (r.blocks && typeof r.blocks === 'object' ? (r.blocks as Record<string, unknown>).compiled : undefined),
    r.compiled,
  ];
  for (const maybeArr of candidateArrays) {
    if (!Array.isArray(maybeArr)) continue;
    for (const entry of maybeArr) {
      const entryRip = parseMaybeNumber(
        typeof entry === 'object' && entry !== null
          ? (entry as Record<string, unknown>).entry_rip ??
              (entry as Record<string, unknown>).entryRip ??
              (entry as Record<string, unknown>).rip
          : entry,
      );
      if (entryRip === rip) return true;
    }
  }

  const candidateMaps: unknown[] = [r.compiled_by_rip, r.compiledByRip];
  for (const maybeMap of candidateMaps) {
    if (!maybeMap || typeof maybeMap !== 'object' || Array.isArray(maybeMap)) continue;
    for (const key of Object.keys(maybeMap as Record<string, unknown>)) {
      const keyRip = parseMaybeNumber(key);
      if (keyRip === rip) return true;
    }
  }

  return false;
}

test('Tier-1 JIT pipeline compiles, installs, and executes a block', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Smoke test currently targets chromium WASM threads support');

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: 'load' });

  const support = await page.evaluate(() => {
    let wasmThreads = false;
    try {
      // eslint-disable-next-line no-new
      new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      wasmThreads = true;
    } catch {
      wasmThreads = false;
    }

    // Empty but valid WASM module header.
    const emptyModule = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    let jitDynamicWasm = false;
    try {
      // `new WebAssembly.Module(...)` tends to fail fast with CSP errors when
      // `script-src` is missing `'wasm-unsafe-eval'`.
      // eslint-disable-next-line no-new
      new WebAssembly.Module(emptyModule);
      jitDynamicWasm = true;
    } catch {
      jitDynamicWasm = false;
    }

    return {
      crossOriginIsolated: globalThis.crossOriginIsolated === true,
      sharedArrayBuffer: typeof SharedArrayBuffer !== 'undefined',
      atomics: typeof Atomics !== 'undefined',
      wasmThreads,
      jitDynamicWasm,
    };
  });

  test.skip(
    !support.crossOriginIsolated || !support.sharedArrayBuffer,
    'SharedArrayBuffer requires COOP/COEP headers (crossOriginIsolated).',
  );
  test.skip(!support.atomics || !support.wasmThreads, 'Shared WebAssembly.Memory (WASM threads) is unavailable.');
  test.skip(!support.jitDynamicWasm, "Dynamic WASM compilation is blocked (missing CSP `script-src 'wasm-unsafe-eval'`).");

  await page.waitForFunction(() => {
    return (window as any).__jit_smoke_result !== undefined;
  }, null, { timeout: 60_000 });

  const result = await page.evaluate(() => (window as any).__jit_smoke_result);
  expect(result).toBeTruthy();
  if (!result || typeof result !== 'object') {
    throw new Error('Missing JIT smoke result object');
  }

  const type = (result as any).type;
  expect(type).toBe('CpuWorkerResult');
  if (type !== 'CpuWorkerResult') {
    throw new Error(`JIT smoke test failed: ${(result as any).reason ?? JSON.stringify(result)}`);
  }

  const interpExecutions =
    firstNumberAtPaths(result, [
      'interp_executions',
      'tier0_executions',
      'tier0_execs',
      'tier0_exec_count',
      'tier0.exec_count',
      'tier0.execCount',
      'tier0_blocks_executed',
      'tier0_blocks',
      'tier0.blocks',
      'tier0.block_count',
      'tier0.blockCount',
      'tier0.blocks_executed',
      'tier0.blocksExecuted',
      'tier0.executions',
    ]) ?? 0;

  const jitExecutions =
    firstNumberAtPaths(result, [
      'jit_executions',
      'tier1_executions',
      'tier1_execs',
      'tier1_exec_count',
      'tier1.exec_count',
      'tier1.execCount',
      'tier1_blocks_executed',
      'tier1_blocks',
      'tier1.blocks',
      'tier1.block_count',
      'tier1.blockCount',
      'tier1.blocks_executed',
      'tier1.blocksExecuted',
      'tier1.executions',
    ]) ?? 0;

  // Tier-0 warmup: ensure we ran at least one interpreted execution before Tier-1 kicked in.
  expect(interpExecutions).toBeGreaterThan(0);
  // Tier-1: ensure at least one JIT-compiled block executed.
  expect(jitExecutions).toBeGreaterThan(0);

  // Verify RIP=0x1000 was compiled+installed.
  expect(hasCompiledRip(result, ENTRY_RIP)).toBe(true);
  expect(findBlockTableIndexByRip(result, ENTRY_RIP)).not.toBeNull();
});
