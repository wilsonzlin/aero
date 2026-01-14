import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import { EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_HUB_PORT_COUNT } from "../usb/uhci_external_hub";

export type UhciRuntimeExternalHubConfig = { guestPath: GuestUsbPath; portCount?: number };

/**
 * Minimal helper for wiring `hid:attachHub` messages into the UHCI runtime (when present).
 *
 * This stays in a pure TS module (instead of being embedded in `io.worker.ts`) so Vitest can cover
 * the behavior without importing a real worker script.
 */
export class UhciRuntimeExternalHubConfigManager {
  #pending: UhciRuntimeExternalHubConfig | null = null;

  setPending(guestPath: GuestUsbPath, portCount: number | undefined): void {
    let clampedPortCount = portCount;
    // Root port 0 hosts the external hub used by both WebHID passthrough and the synthetic HID
    // devices (keyboard/mouse/gamepad/consumer-control). Never configure the hub with fewer
    // downstream ports than that reserved synthetic range (ports 1..UHCI_SYNTHETIC_HID_HUB_PORT_COUNT),
    // otherwise those devices may fail to attach.
    if (guestPath.length === 1 && guestPath[0] === EXTERNAL_HUB_ROOT_PORT && clampedPortCount !== undefined) {
      if (!Number.isFinite(clampedPortCount)) {
        clampedPortCount = UHCI_SYNTHETIC_HID_HUB_PORT_COUNT;
      } else {
        clampedPortCount = Math.max(
          UHCI_SYNTHETIC_HID_HUB_PORT_COUNT,
          Math.min(255, Math.floor(clampedPortCount)),
        );
      }
    }
    this.#pending = { guestPath, ...(clampedPortCount !== undefined ? { portCount: clampedPortCount } : {}) };
  }

  get pending(): UhciRuntimeExternalHubConfig | null {
    return this.#pending;
  }

  apply(runtime: unknown, opts?: { warn?: (message: string, err: unknown) => void }): void {
    const cfg = this.#pending;
    if (!cfg) return;
    if (!runtime) return;
    const runtimeAny = runtime as unknown as Record<string, unknown>;
    const fn = runtimeAny.webhid_attach_hub ?? runtimeAny.webhidAttachHub;
    if (typeof fn !== "function") return;
    try {
      fn.call(runtime, cfg.guestPath, cfg.portCount);
    } catch (err) {
      opts?.warn?.("Failed to configure UHCI runtime external hub", err);
    }
  }
}
