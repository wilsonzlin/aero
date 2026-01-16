import { describe, expect, it, vi } from "vitest";

import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import { MAX_REMOTE_URL_LEN } from "./url_limits";
import type { RuntimeDiskRequestMessage } from "./runtime_disk_protocol";

describe("RuntimeDiskWorker (remote URL length limits)", () => {
  it("rejects oversized openRemote url before calling fetch", async () => {
    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async () => {
      throw new Error("fetch should not be called for oversized openRemote url");
    });
    try {
      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "openRemote",
        payload: { url: "a".repeat(MAX_REMOTE_URL_LEN + 1) },
      } satisfies RuntimeDiskRequestMessage);
    } finally {
      fetchSpy.mockRestore();
    }

    const resp = posted.shift();
    expect(resp?.ok).toBe(false);
    expect(String(resp?.error?.message ?? "")).toMatch(/too long/i);
  });

  it("rejects oversized openChunked manifestUrl before calling fetch", async () => {
    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async () => {
      throw new Error("fetch should not be called for oversized openChunked manifestUrl");
    });
    try {
      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "openChunked",
        payload: { manifestUrl: "a".repeat(MAX_REMOTE_URL_LEN + 1) },
      } satisfies RuntimeDiskRequestMessage);
    } finally {
      fetchSpy.mockRestore();
    }

    const resp = posted.shift();
    expect(resp?.ok).toBe(false);
    expect(String(resp?.error?.message ?? "")).toMatch(/too long/i);
  });

  it("rejects oversized remote open spec url before calling fetch", async () => {
    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async () => {
      throw new Error("fetch should not be called for oversized remote open spec url");
    });
    try {
      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "open",
        payload: {
          spec: {
            kind: "remote",
            remote: {
              delivery: "range",
              kind: "cd",
              format: "iso",
              url: "a".repeat(MAX_REMOTE_URL_LEN + 1),
              cacheKey: "k",
            },
          },
        },
      } satisfies RuntimeDiskRequestMessage);
    } finally {
      fetchSpy.mockRestore();
    }

    const resp = posted.shift();
    expect(resp?.ok).toBe(false);
    expect(String(resp?.error?.message ?? "")).toMatch(/too long/i);
  });

  it("rejects oversized remote open spec manifestUrl before calling fetch", async () => {
    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async () => {
      throw new Error("fetch should not be called for oversized remote open spec manifestUrl");
    });
    try {
      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "open",
        payload: {
          spec: {
            kind: "remote",
            remote: {
              delivery: "chunked",
              kind: "hdd",
              format: "raw",
              manifestUrl: "a".repeat(MAX_REMOTE_URL_LEN + 1),
              cacheKey: "k",
            },
          },
        },
      } satisfies RuntimeDiskRequestMessage);
    } finally {
      fetchSpy.mockRestore();
    }

    const resp = posted.shift();
    expect(resp?.ok).toBe(false);
    expect(String(resp?.error?.message ?? "")).toMatch(/too long/i);
  });
});

