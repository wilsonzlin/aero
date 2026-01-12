import { afterEach, describe, expect, it, vi } from "vitest";

import { resolveAOverDoh, resolveAOverDohJson } from "./doh";

const originalFetch = globalThis.fetch;

afterEach(() => {
  (globalThis as typeof globalThis & { fetch: typeof fetch }).fetch = originalFetch;
});

describe("resolveAOverDoh", () => {
  it("returns null for oversized application/dns-message responses", async () => {
    const fetchFn = vi.fn(async () => {
      return new Response(new Uint8Array([0, 1, 2]), {
        status: 200,
        headers: {
          "content-type": "application/dns-message",
          // Force the size check to fail before reading.
          "content-length": String(1024 * 1024 + 1),
        },
      });
    });
    (globalThis as typeof globalThis & { fetch: typeof fetch }).fetch = fetchFn as unknown as typeof fetch;

    await expect(resolveAOverDoh("example.com")).resolves.toBeNull();
    expect(fetchFn).toHaveBeenCalledTimes(1);
  });
});

describe("resolveAOverDohJson", () => {
  it("returns null for oversized JSON responses", async () => {
    const fetchFn = vi.fn(async () => {
      return new Response("{}", {
        status: 200,
        headers: {
          "content-type": "application/dns-json",
          "content-length": String(1024 * 1024 + 1),
        },
      });
    });
    (globalThis as typeof globalThis & { fetch: typeof fetch }).fetch = fetchFn as unknown as typeof fetch;

    await expect(resolveAOverDohJson("example.com")).resolves.toBeNull();
    expect(fetchFn).toHaveBeenCalledTimes(1);
  });
});

