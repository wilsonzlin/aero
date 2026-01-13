import type { InputBackend } from "./input_backend_selection";

/**
 * Stable integer encoding for {@link InputBackend} values stored in the shared
 * runtime status SAB (`web/src/runtime/shared_layout.ts`).
 *
 * IMPORTANT: These values are part of the runtime ABI between workers and the
 * main thread. Do not reorder or reuse codes.
 */
export const INPUT_BACKEND_STATUS_CODE = {
  ps2: 0,
  usb: 1,
  virtio: 2,
} as const;

export type InputBackendStatusCode = (typeof INPUT_BACKEND_STATUS_CODE)[keyof typeof INPUT_BACKEND_STATUS_CODE];

export function encodeInputBackendStatus(backend: InputBackend): InputBackendStatusCode {
  switch (backend) {
    case "ps2":
      return INPUT_BACKEND_STATUS_CODE.ps2;
    case "usb":
      return INPUT_BACKEND_STATUS_CODE.usb;
    case "virtio":
      return INPUT_BACKEND_STATUS_CODE.virtio;
    default: {
      const neverBackend: never = backend;
      throw new Error(`Unknown input backend: ${String(neverBackend)}`);
    }
  }
}

export function decodeInputBackendStatus(code: number): InputBackend | null {
  switch (code | 0) {
    case INPUT_BACKEND_STATUS_CODE.ps2:
      return "ps2";
    case INPUT_BACKEND_STATUS_CODE.usb:
      return "usb";
    case INPUT_BACKEND_STATUS_CODE.virtio:
      return "virtio";
    default:
      return null;
  }
}

