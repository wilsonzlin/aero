// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";

import { mountSettingsPanel } from "./settings_panel";
import type { PlatformFeatureReport } from "../platform/features";

function makeReport(partial: Partial<PlatformFeatureReport>): PlatformFeatureReport {
  return {
    crossOriginIsolated: true,
    sharedArrayBuffer: true,
    wasmSimd: true,
    wasmThreads: true,
    webgpu: true,
    webusb: false,
    webhid: false,
    webgl2: true,
    opfs: true,
    opfsSyncAccessHandle: true,
    audioWorklet: true,
    offscreenCanvas: true,
    jit_dynamic_wasm: true,
    ...partial,
  };
}

function findRowByLabelText(container: HTMLElement, labelText: string): HTMLDivElement {
  const labels = Array.from(container.querySelectorAll("label"));
  const match = labels.find((label) => (label.querySelector("span")?.textContent ?? "") === labelText);
  if (!match) throw new Error(`Missing settings row for label: ${labelText}`);
  const row = match.parentElement;
  if (!row || row.tagName !== "DIV") throw new Error(`Unexpected DOM shape for row: ${labelText}`);
  return row as HTMLDivElement;
}

function createFakeConfigManager(): any {
  const state = {
    effective: {
      guestMemoryMiB: 512,
      vramMiB: 32,
      enableWorkers: true,
      enableWebGPU: true,
      proxyUrl: null,
      activeDiskImage: null,
      logLevel: "info",
      vmRuntime: "legacy",
      virtioNetMode: "modern",
      virtioInputMode: "modern",
      virtioSndMode: "modern",
      forceKeyboardBackend: "auto",
      forceMouseBackend: "auto",
    },
    lockedKeys: new Set(),
    capabilities: {
      supportsThreadedWorkers: true,
      threadedWorkersUnsupportedReason: null,
      supportsWebGPU: true,
      webgpuUnsupportedReason: null,
    },
  };

  return {
    updateStoredConfig: vi.fn(),
    resetToDefaults: vi.fn(),
    getState: () => state,
    subscribe: (listener: (state: any) => void) => {
      listener(state);
      return () => {};
    },
  };
}

describe("ui/settings_panel", () => {
  it("disables vmRuntime=machine when OPFS SyncAccessHandle is unavailable and shows a hint", () => {
    const host = document.createElement("div");
    const manager = createFakeConfigManager();
    mountSettingsPanel(host, manager, makeReport({ opfsSyncAccessHandle: false }));

    const row = findRowByLabelText(host, "VM runtime");
    const select = row.querySelector("select") as HTMLSelectElement;
    expect(select).toBeTruthy();

    const machineOption = Array.from(select.options).find((o) => o.value === "machine");
    expect(machineOption).toBeTruthy();
    expect(machineOption!.disabled).toBe(true);

    const hint = row.querySelector(".hint") as HTMLElement;
    expect(hint.textContent).toContain("OPFS SyncAccessHandle");
    expect(hint.textContent).toContain("machine runtime is disabled");
  });

  it("enables vmRuntime=machine when OPFS SyncAccessHandle is available", () => {
    const host = document.createElement("div");
    const manager = createFakeConfigManager();
    mountSettingsPanel(host, manager, makeReport({ opfsSyncAccessHandle: true }));

    const row = findRowByLabelText(host, "VM runtime");
    const select = row.querySelector("select") as HTMLSelectElement;
    expect(select).toBeTruthy();

    const machineOption = Array.from(select.options).find((o) => o.value === "machine");
    expect(machineOption).toBeTruthy();
    expect(machineOption!.disabled).toBe(false);

    const hint = row.querySelector(".hint") as HTMLElement;
    expect(hint.textContent).toContain("requires OPFS SyncAccessHandle");
    expect(hint.textContent).not.toContain("machine runtime is disabled");
  });
});

