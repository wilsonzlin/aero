import { describe, expect, it, vi } from "vitest";

import {
  DiskAccessLeaseRefresher,
  MAX_STREAM_LEASE_JSON_BYTES,
  MAX_TIMEOUT_MS,
  createDiskAccessLeaseFromLeaseEndpoint,
  fetchWithDiskAccessLease,
  fetchWithDiskAccessLeaseForUrl,
  type DiskAccessLease,
} from "./disk_access_lease";

describe("DiskAccessLeaseRefresher", () => {
  it("proactively refreshes before expiry", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const lease: DiskAccessLease = {
      url: "https://cdn.example.test/disk.img?sig=1",
      credentialsMode: "omit",
      expiresAt: new Date(10_000),
      refresh: async () => lease,
    };
    const refresh = vi.spyOn(lease, "refresh");

    const refresher = new DiskAccessLeaseRefresher(lease, { refreshMarginMs: 1_000 });
    refresher.start();

    try {
      await vi.advanceTimersByTimeAsync(8_999);
      expect(refresh).toHaveBeenCalledTimes(0);

      await vi.advanceTimersByTimeAsync(1);
      expect(refresh).toHaveBeenCalledTimes(1);
    } finally {
      refresher.stop();
      vi.useRealTimers();
    }
  });

  it("does not overflow timers when expiresAt is far in the future", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const expiryMs = 100 * 24 * 60 * 60 * 1000; // 100 days
    const lease: DiskAccessLease = {
      url: "https://cdn.example.test/disk.img?sig=1",
      credentialsMode: "omit",
      expiresAt: new Date(expiryMs),
      refresh: async () => lease,
    };
    const refresh = vi.spyOn(lease, "refresh").mockImplementation(async () => {
      // Prevent an immediate re-refresh loop after expiry by extending the lease.
      lease.expiresAt = new Date(expiryMs + 1_000);
      return lease;
    });
    const setTimeoutSpy = vi.spyOn(globalThis, "setTimeout");

    const refresher = new DiskAccessLeaseRefresher(lease, { refreshMarginMs: 0 });
    refresher.start();

    try {
      // First schedule should clamp to MAX_TIMEOUT_MS (check timer).
      expect(setTimeoutSpy.mock.calls[0]?.[1]).toBe(MAX_TIMEOUT_MS);

      await vi.advanceTimersByTimeAsync(MAX_TIMEOUT_MS);
      expect(refresh).toHaveBeenCalledTimes(0);

      // Check timer should have rescheduled another MAX_TIMEOUT_MS check.
      expect(setTimeoutSpy.mock.calls[1]?.[1]).toBe(MAX_TIMEOUT_MS);
      await vi.advanceTimersByTimeAsync(MAX_TIMEOUT_MS);
      expect(refresh).toHaveBeenCalledTimes(0);

      // Fast-forward to just before expiry; refresh must not have happened early.
      await vi.advanceTimersByTimeAsync(expiryMs - 2 * MAX_TIMEOUT_MS - 1);
      expect(refresh).toHaveBeenCalledTimes(0);

      // At expiry, refresh should fire exactly once.
      await vi.advanceTimersByTimeAsync(1);
      expect(refresh).toHaveBeenCalledTimes(1);
    } finally {
      refresher.stop();
      setTimeoutSpy.mockRestore();
      vi.useRealTimers();
    }
  });
});

describe("fetchWithDiskAccessLease", () => {
  it("refreshes and retries once on 401/403", async () => {
    const fetchFn = vi
      .fn<[RequestInfo | URL, RequestInit?], Promise<Response>>()
      .mockResolvedValueOnce(new Response("forbidden", { status: 403 }))
      .mockResolvedValueOnce(new Response(new Uint8Array([1, 2, 3]), { status: 206 }));

    const lease: DiskAccessLease = {
      url: "https://cdn.example.test/disk.img?sig=1",
      credentialsMode: "omit",
      refresh: async () => lease,
    };
    const refresh = vi.spyOn(lease, "refresh").mockImplementation(async () => {
      lease.url = "https://cdn.example.test/disk.img?sig=2";
      return lease;
    });

    const resp = await fetchWithDiskAccessLease(lease, { method: "GET" }, { fetch: fetchFn });

    expect(refresh).toHaveBeenCalledTimes(1);
    expect(fetchFn).toHaveBeenCalledTimes(2);
    expect(fetchFn.mock.calls[0]?.[0]).toBe("https://cdn.example.test/disk.img?sig=1");
    expect(fetchFn.mock.calls[1]?.[0]).toBe("https://cdn.example.test/disk.img?sig=2");
    expect(resp.status).toBe(206);
  });

  it("does not retry more than once", async () => {
    const fetchFn = vi
      .fn<[RequestInfo | URL, RequestInit?], Promise<Response>>()
      .mockResolvedValueOnce(new Response("forbidden", { status: 403 }))
      .mockResolvedValueOnce(new Response("forbidden", { status: 403 }));

    const lease: DiskAccessLease = {
      url: "https://cdn.example.test/disk.img?sig=1",
      credentialsMode: "omit",
      refresh: async () => lease,
    };
    const refresh = vi.spyOn(lease, "refresh");

    const resp = await fetchWithDiskAccessLease(lease, { method: "GET" }, { fetch: fetchFn });

    expect(refresh).toHaveBeenCalledTimes(1);
    expect(fetchFn).toHaveBeenCalledTimes(2);
    expect(resp.status).toBe(403);
  });
});

describe("fetchWithDiskAccessLeaseForUrl", () => {
  it("recomputes the request URL after refresh when given a URL provider", async () => {
    const fetchFn = vi
      .fn<[RequestInfo | URL, RequestInit?], Promise<Response>>()
      .mockResolvedValueOnce(new Response("forbidden", { status: 403 }))
      .mockResolvedValueOnce(new Response("ok", { status: 200 }));

    const lease: DiskAccessLease = {
      url: "https://cdn.example.test/base?sig=1",
      credentialsMode: "omit",
      refresh: async () => lease,
    };
    const refresh = vi.spyOn(lease, "refresh").mockImplementation(async () => {
      lease.url = "https://cdn.example.test/base?sig=2";
      return lease;
    });

    const resp = await fetchWithDiskAccessLeaseForUrl(lease, () => `${lease.url}&chunk=1`, { method: "GET" }, { fetch: fetchFn });

    expect(refresh).toHaveBeenCalledTimes(1);
    expect(fetchFn).toHaveBeenCalledTimes(2);
    expect(fetchFn.mock.calls[0]?.[0]).toBe("https://cdn.example.test/base?sig=1&chunk=1");
    expect(fetchFn.mock.calls[1]?.[0]).toBe("https://cdn.example.test/base?sig=2&chunk=1");
    expect(resp.status).toBe(200);
  });
});

describe("createDiskAccessLeaseFromLeaseEndpoint", () => {
  it("rejects oversized lease responses before JSON parsing", async () => {
    const fetchFn = vi.fn<[RequestInfo | URL, RequestInit?], Promise<Response>>().mockResolvedValue(
      new Response("{}", {
        status: 200,
        headers: {
          "content-type": "application/json",
          "content-length": String(MAX_STREAM_LEASE_JSON_BYTES + 1),
        },
      }),
    );

    const lease = createDiskAccessLeaseFromLeaseEndpoint("/lease", { delivery: "range", fetchFn });
    await expect(lease.refresh()).rejects.toThrow(/too large/i);
  });

  it("does not accept required fields inherited from Object.prototype", async () => {
    const fetchFn = vi.fn<[RequestInfo | URL, RequestInit?], Promise<Response>>().mockResolvedValue(
      new Response(JSON.stringify({}), { status: 200, headers: { "content-type": "application/json" } }),
    );

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "url");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }
    try {
      Object.defineProperty(Object.prototype, "url", {
        value: "https://cdn.example.test/disk.img?sig=proto",
        configurable: true,
      });
      const lease = createDiskAccessLeaseFromLeaseEndpoint("/lease", { delivery: "range", fetchFn });
      await expect(lease.refresh()).rejects.toThrow(/stream lease response url/i);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "url", existing);
      else Reflect.deleteProperty(Object.prototype, "url");
    }
  });

  it("does not accept optional nested fields inherited from Object.prototype", async () => {
    const fetchFn = vi.fn<[RequestInfo | URL, RequestInit?], Promise<Response>>().mockResolvedValue(
      // Valid top-level url, but missing `chunked` (required for delivery=chunked).
      new Response(JSON.stringify({ url: "https://cdn.example.test/ignored" }), { status: 200 }),
    );

    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "chunked");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }
    try {
      Object.defineProperty(Object.prototype, "chunked", {
        value: { delivery: "chunked", manifestUrl: "/evil" },
        configurable: true,
      });
      const lease = createDiskAccessLeaseFromLeaseEndpoint("/lease", { delivery: "chunked", fetchFn });
      await expect(lease.refresh()).rejects.toThrow(/missing chunked\.manifestUrl/i);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "chunked", existing);
      else Reflect.deleteProperty(Object.prototype, "chunked");
    }
  });
});
