// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";

import { installNetTraceUI } from "./trace_ui";

describe("net/trace_ui", () => {
  it("polls getStats while mounted and stops after disconnect", async () => {
    vi.useFakeTimers();
    try {
      const host = document.createElement("div");
      document.body.appendChild(host);

      const backend = {
        isEnabled: () => false,
        enable: () => {},
        disable: () => {},
        downloadPcapng: async () => new Uint8Array(),
        getStats: vi.fn(() => ({ enabled: false, records: 1, bytes: 2 })),
        clear: () => {},
      };

      installNetTraceUI(host, backend);

      // Initial poll is triggered immediately.
      await Promise.resolve();
      expect(backend.getStats).toHaveBeenCalledTimes(1);

      await vi.advanceTimersByTimeAsync(500);
      expect(backend.getStats).toHaveBeenCalledTimes(2);

      await vi.advanceTimersByTimeAsync(1000);
      expect(backend.getStats).toHaveBeenCalledTimes(4);

      host.remove();

      // One more interval tick should notice the disconnect and abort without
      // calling getStats again, then clear the interval.
      await vi.advanceTimersByTimeAsync(500);
      const callsAfterDisconnect = backend.getStats.mock.calls.length;

      await vi.advanceTimersByTimeAsync(2000);
      expect(backend.getStats.mock.calls.length).toBe(callsAfterDisconnect);
    } finally {
      vi.useRealTimers();
      document.body.innerHTML = "";
    }
  });

  it("shows a Clear capture button when clear() exists and invokes it", async () => {
    const host = document.createElement("div");
    document.body.appendChild(host);

    const clear = vi.fn();
    const backend = {
      isEnabled: () => false,
      enable: () => {},
      disable: () => {},
      downloadPcapng: async () => new Uint8Array(),
      clear,
    };

    installNetTraceUI(host, backend);

    const buttons = Array.from(host.querySelectorAll("button"));
    const clearBtn = buttons.find((b) => (b.textContent ?? "").includes("Clear capture"));
    expect(clearBtn).toBeTruthy();

    clearBtn!.click();
    // `clear()` is called before the first `await` yields.
    expect(clear).toHaveBeenCalledTimes(1);
  });

  it("writes getStats errors to the status <pre>", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);

    const backend = {
      isEnabled: () => false,
      enable: () => {},
      disable: () => {},
      downloadPcapng: async () => new Uint8Array(),
      getStats: () => {
        throw new Error("boom");
      },
    };

    installNetTraceUI(host, backend);

    const wrapper = host.querySelector(".net-trace");
    expect(wrapper).toBeTruthy();
    const status = wrapper!.lastElementChild as HTMLElement;
    expect(status.tagName).toBe("PRE");
    expect(status.textContent).toContain("boom");
  });
});

