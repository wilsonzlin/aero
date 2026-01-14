// Test-only presenter module: delays the optional telemetry hooks (drain_gpu_events) so unit tests
// can assert `vm.snapshot.pause` waits for in-flight telemetry polling before acknowledging.

postMessage({ type: "mock_presenter_loaded" });

export function present(): boolean {
  return true;
}

export async function drain_gpu_events(): Promise<unknown[]> {
  postMessage({ type: "mock_telemetry_started" });
  await new Promise((resolve) => setTimeout(resolve, 200));
  postMessage({ type: "mock_telemetry_finished" });
  return [];
}

