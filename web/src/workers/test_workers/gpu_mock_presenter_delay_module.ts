// Test-only presenter module: `present()` returns a Promise that resolves after a short delay.
//
// This is used to ensure snapshot pause waits for in-flight tick/present work before
// acknowledging `vm.snapshot.paused`.

// Signal that the module has been imported (so tests know `presentFn` is installed).
postMessage({ type: "mock_presenter_loaded" });

export async function present(): Promise<boolean> {
  postMessage({ type: "mock_present_started" });
  await new Promise((resolve) => setTimeout(resolve, 200));
  postMessage({ type: "mock_present_finished" });
  return true;
}

