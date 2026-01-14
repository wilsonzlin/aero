import { describe, expect, it, vi } from "vitest";

import { UHCI_SYNTHETIC_HID_HUB_PORT_COUNT } from "../usb/uhci_external_hub";
import { UhciRuntimeExternalHubConfigManager } from "./uhci_runtime_hub_config";

describe("workers/uhci_runtime_hub_config", () => {
  it("invokes runtime webhid_attach_hub when available", () => {
    const manager = new UhciRuntimeExternalHubConfigManager();
    manager.setPending([0], 16);

    const webhid_attach_hub = vi.fn();
    const runtime = { webhid_attach_hub };
    manager.apply(runtime);

    expect(webhid_attach_hub).toHaveBeenCalledWith([0], 16);
  });

  it("accepts camelCase webhidAttachHub export (backwards compatibility)", () => {
    const manager = new UhciRuntimeExternalHubConfigManager();
    manager.setPending([0], 16);

    const webhidAttachHub = vi.fn();
    const runtime = { webhidAttachHub };
    manager.apply(runtime);

    expect(webhidAttachHub).toHaveBeenCalledWith([0], 16);
  });

  it("clamps external hub port count so it cannot shrink below the reserved synthetic HID range", () => {
    const manager = new UhciRuntimeExternalHubConfigManager();
    manager.setPending([0], 1);

    const webhid_attach_hub = vi.fn();
    const runtime = { webhid_attach_hub };
    manager.apply(runtime);

    expect(webhid_attach_hub).toHaveBeenCalledWith([0], UHCI_SYNTHETIC_HID_HUB_PORT_COUNT);
  });

  it("is a no-op when the runtime does not expose webhid_attach_hub", () => {
    const manager = new UhciRuntimeExternalHubConfigManager();
    manager.setPending([0], 4);

    const runtime = {};
    expect(() => manager.apply(runtime)).not.toThrow();
  });

  it("reports errors via the warn callback when webhid_attach_hub throws", () => {
    const manager = new UhciRuntimeExternalHubConfigManager();
    manager.setPending([0], 8);

    const webhid_attach_hub = vi.fn(() => {
      throw new Error("boom");
    });
    const runtime = { webhid_attach_hub };

    const warn = vi.fn();
    manager.apply(runtime, { warn });

    expect(webhid_attach_hub).toHaveBeenCalledWith([0], 8);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(warn.mock.calls[0]?.[0]).toContain("Failed to configure UHCI runtime external hub");
  });
});
