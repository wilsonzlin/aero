// Test-only presenter module: reports whether shared-state globals are wired up.
//
// Used to verify snapshot pause across GPU worker init:
// - when init runs while snapshot-paused, __aeroScanoutState/__aeroCursorState should remain disabled
// - after resume, they should be restored.

const scanoutOk = (globalThis as unknown as { __aeroScanoutState?: unknown }).__aeroScanoutState instanceof Int32Array;
const cursorOk = (globalThis as unknown as { __aeroCursorState?: unknown }).__aeroCursorState instanceof Int32Array;

postMessage({ type: "mock_presenter_globals_probe", phase: "import", scanoutOk, cursorOk });

export function present(): boolean {
  const scanoutOk2 = (globalThis as unknown as { __aeroScanoutState?: unknown }).__aeroScanoutState instanceof Int32Array;
  const cursorOk2 = (globalThis as unknown as { __aeroCursorState?: unknown }).__aeroCursorState instanceof Int32Array;
  postMessage({ type: "mock_presenter_globals_probe", phase: "present", scanoutOk: scanoutOk2, cursorOk: cursorOk2 });
  return true;
}

