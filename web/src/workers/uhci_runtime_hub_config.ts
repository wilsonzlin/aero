import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";

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
    this.#pending = { guestPath, ...(portCount !== undefined ? { portCount } : {}) };
  }

  get pending(): UhciRuntimeExternalHubConfig | null {
    return this.#pending;
  }

  apply(runtime: unknown, opts?: { warn?: (message: string, err: unknown) => void }): void {
    const cfg = this.#pending;
    if (!cfg) return;
    if (!runtime) return;
    const fn = (runtime as { webhid_attach_hub?: unknown }).webhid_attach_hub;
    if (typeof fn !== "function") return;
    try {
      fn.call(runtime, cfg.guestPath, cfg.portCount);
    } catch (err) {
      opts?.warn?.("Failed to configure UHCI runtime external hub", err);
    }
  }
}

