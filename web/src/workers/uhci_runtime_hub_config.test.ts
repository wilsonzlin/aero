import { describe, expect, it, vi } from "vitest";

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

