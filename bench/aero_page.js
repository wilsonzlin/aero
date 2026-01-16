const DISABLE_ANIMATIONS_CSS = `
*,
*::before,
*::after {
  animation-delay: 0s !important;
  animation-duration: 0s !important;
  animation-iteration-count: 1 !important;
  scroll-behavior: auto !important;
  transition-delay: 0s !important;
  transition-duration: 0s !important;
}
`;

/**
 * @param {import('playwright').Browser} browser
 * @param {{ viewport: { width: number, height: number } }} opts
 */
export async function createAeroContext(browser, opts) {
  const context = await browser.newContext({
    viewport: opts.viewport,
    deviceScaleFactor: 1,
    reducedMotion: 'reduce',
    colorScheme: 'light',
    locale: 'en-US',
    timezoneId: 'UTC'
  });

  await context.addInitScript(
    ({ cssText }) => {
      const style = document.createElement('style');
      style.textContent = cssText;
      const attach = () => {
        (document.head || document.documentElement).appendChild(style);
      };
      if (document.head) attach();
      else document.addEventListener('DOMContentLoaded', attach, { once: true });
    },
    { cssText: DISABLE_ANIMATIONS_CSS }
  );

  return context;
}

/**
 * @param {import('playwright').Browser} browser
 * @param {{ viewport: { width: number, height: number } }} opts
 */
export async function createAeroPage(browser, opts) {
  const context = await createAeroContext(browser, opts);
  const page = await context.newPage();
  return { context, page };
}

/**
 * @param {import('playwright').Page} page
 * @param {number} timeoutMs
 */
export async function waitForAeroReady(page, timeoutMs) {
  await page.waitForFunction(
    async () => {
      // eslint-disable-next-line no-undef
      const aero = window.aero;
      if (!aero) return false;

      if (typeof aero.isReady === 'function') {
        const v = aero.isReady();
        // Some implementations may return a boolean; others return a Promise<boolean>.
        return typeof v === 'boolean' ? v : await v;
      }

      if (aero.ready === true) return true;
      if (aero.whenReady && typeof aero.whenReady.then === 'function') {
        await aero.whenReady;
        return true;
      }

      return false;
    },
    { timeout: timeoutMs, polling: 50 }
  );
}

/**
 * @param {import('playwright').Page} page
 */
export async function resetPerf(page) {
  await page
    .evaluate(() => {
      // eslint-disable-next-line no-undef
      window.aero?.perf?.reset?.();
    })
    .catch(() => undefined);
}

/**
 * @param {import('playwright').Page} page
 * @returns {Promise<any>}
 */
export async function exportPerf(page) {
  return await page.evaluate(async () => {
    // eslint-disable-next-line no-undef
    const aero = window.aero;
    if (!aero?.perf?.export || typeof aero.perf.export !== 'function') {
      throw new Error('window.aero.perf.export() is not available');
    }
    return await aero.perf.export();
  });
}

/**
 * Best-effort extraction for wasm timings from a perf export.
 * The schema of `window.aero.perf.export()` is owned by the app and may evolve.
 *
 * @param {any} perfExport
 * @returns {{ wasmCompileMs?: number, wasmInstantiateMs?: number }}
 */
export function extractWasmTimes(perfExport) {
  const result = {};
  const wasm =
    perfExport?.wasm ??
    perfExport?.metrics?.wasm ??
    perfExport?.wasmTimings ??
    perfExport?.timings?.wasm ??
    null;

  if (!wasm || typeof wasm !== 'object') return result;

  const compileMs = wasm.compileMs ?? wasm.compile ?? wasm.compileTimeMs ?? wasm.compile_duration_ms ?? undefined;
  const instantiateMs =
    wasm.instantiateMs ?? wasm.instantiate ?? wasm.instantiateTimeMs ?? wasm.instantiate_duration_ms ?? undefined;

  if (Number.isFinite(compileMs)) result.wasmCompileMs = Number(compileMs);
  if (Number.isFinite(instantiateMs)) result.wasmInstantiateMs = Number(instantiateMs);

  return result;
}
