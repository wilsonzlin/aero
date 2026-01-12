import { describe, expect, it, vi } from "vitest";

import { ImageGatewayClient } from "./image_gateway";

describe("ImageGatewayClient DiskAccessLease mapping", () => {
  it("maps cookie auth to credentialsMode=include", async () => {
    const fetchFn = vi.fn(async () => {
      return new Response(
        JSON.stringify({
          url: "https://cdn.example.test/disk.img",
          auth: { type: "cookie", expiresAt: "2026-01-10T00:00:00Z", cookies: [] },
          size: 123,
          etag: '"abc"',
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });

    const client = new ImageGatewayClient({ fetch: fetchFn, baseUrl: "https://gw.example.test" });
    const lease = await client.createDiskAccessLease("img1");

    expect(lease.url).toBe("https://cdn.example.test/disk.img");
    expect(lease.credentialsMode).toBe("include");
    expect(lease.expiresAt?.toISOString()).toBe("2026-01-10T00:00:00.000Z");
  });

  it("maps signed url auth to credentialsMode=omit", async () => {
    const fetchFn = vi.fn(async () => {
      return new Response(
        JSON.stringify({
          url: "https://cdn.example.test/disk.img?Policy=...&Signature=...",
          auth: { type: "url", expiresAt: "2026-01-10T00:00:00Z" },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });

    const client = new ImageGatewayClient({ fetch: fetchFn, baseUrl: "https://gw.example.test" });
    const lease = await client.createDiskAccessLease("img1");

    expect(lease.credentialsMode).toBe("omit");
    expect(lease.expiresAt?.toISOString()).toBe("2026-01-10T00:00:00.000Z");
  });

  it("rejects oversized JSON responses", async () => {
    const fetchFn = vi.fn(async () => {
      return new Response("{}", {
        status: 200,
        headers: {
          "content-type": "application/json",
          "content-length": String(1024 * 1024 + 1),
        },
      });
    });

    const client = new ImageGatewayClient({ fetch: fetchFn, baseUrl: "https://gw.example.test" });
    await expect(client.createDiskAccessLease("img1")).rejects.toHaveProperty("name", "ResponseTooLargeError");
  });
});
