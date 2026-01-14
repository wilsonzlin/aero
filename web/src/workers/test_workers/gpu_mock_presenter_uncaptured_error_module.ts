import { PresenterError } from "../../gpu/presenter";

// Signal that the module has been imported (so tests know `presentFn` is installed).
postMessage({ type: "mock_presenter_loaded" });

let callCount = 0;

export function present(): boolean {
  callCount += 1;

  postMessage({ type: "mock_present_call", callCount });

  if (callCount === 1) {
    // Simulate a WebGPU uncaptured validation error. The GPU worker should surface this via a
    // structured `events` message but should not treat it as a fatal worker error (no restart).
    throw new PresenterError("webgpu_uncaptured_error", "simulated uncaptured error", {
      name: "GPUValidationError",
      message: "validation error",
    });
  }

  postMessage({ type: "mock_present", ok: true, callCount });
  return true;
}

