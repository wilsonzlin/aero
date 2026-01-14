import { PresenterError } from "../../gpu/presenter";

// Signal that the module has been imported (so tests know `presentFn` is installed).
postMessage({ type: "mock_presenter_loaded" });

let callCount = 0;

export function present(dirtyRects?: unknown): boolean {
  callCount += 1;

  const dirty =
    dirtyRects == null
      ? null
      : Array.isArray(dirtyRects)
        ? dirtyRects.length
        : typeof dirtyRects === "object"
          ? 1
          : 0;

  // Emit a message for every invocation so tests can assert that a present pass ran even when the
  // legacy shared framebuffer is idle.
  postMessage({ type: "mock_present_call", callCount, dirtyRects: dirty });

  if (callCount === 1) {
    // Simulate a device loss that should trigger the GPU worker recovery path.
    throw new PresenterError("webgpu_device_lost", "simulated device lost");
  }

  postMessage({ type: "mock_present", ok: true, callCount });
  return true;
}

