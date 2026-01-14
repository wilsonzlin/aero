// Test-only presenter module: exposes whether snapshot pause/resume correctly preserves
// the shared-state globals (__aeroScanoutState/__aeroCursorState) even when a resume races
// with an in-flight pause (coordinator timeout scenario).

postMessage({ type: "mock_presenter_loaded" });

export async function present(): Promise<boolean> {
  const scanoutOk = (globalThis as unknown as { __aeroScanoutState?: unknown }).__aeroScanoutState instanceof Int32Array;
  const cursorOk = (globalThis as unknown as { __aeroCursorState?: unknown }).__aeroCursorState instanceof Int32Array;
  postMessage({ type: "mock_present_globals", scanoutOk, cursorOk });
  await new Promise((resolve) => setTimeout(resolve, 200));
  postMessage({ type: "mock_present_finished" });
  return true;
}

