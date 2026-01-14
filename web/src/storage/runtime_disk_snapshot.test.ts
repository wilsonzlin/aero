import { describe, expect, it } from "vitest";

import {
  deserializeRuntimeDiskSnapshot,
  serializeRuntimeDiskSnapshot,
  shouldInvalidateRemoteCache,
  shouldInvalidateRemoteOverlay,
  type RemoteCacheBinding,
  type RemoteDiskBaseSnapshot,
  type RuntimeDiskSnapshot,
} from "./runtime_disk_snapshot";
import { remoteRangeDeliveryType } from "./remote_cache_manager";
import { DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES } from "./metadata";

function encodeJson(value: unknown): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(value));
}

function sampleSnapshot(): RuntimeDiskSnapshot {
  return {
    version: 1,
    nextHandle: 3,
    disks: [
      {
        handle: 1,
        readOnly: false,
        sectorSize: 512,
        capacityBytes: 20 * 1024 * 1024,
        backend: {
          kind: "local",
          backend: "opfs",
          key: "abc.aerospar",
          format: "aerospar",
          diskKind: "hdd",
          sizeBytes: 20 * 1024 * 1024,
          overlay: {
            fileName: "abc.overlay.aerospar",
            diskSizeBytes: 20 * 1024 * 1024,
            blockSizeBytes: 1024 * 1024,
          },
        },
      },
      {
        handle: 2,
        readOnly: false,
        sectorSize: 512,
        capacityBytes: 20 * 1024 * 1024,
        backend: {
          kind: "remote",
          backend: "opfs",
          diskKind: "hdd",
          sizeBytes: 20 * 1024 * 1024,
          base: {
            imageId: "win7-sp1-x64",
            version: "sha256-deadbeef",
            deliveryType: remoteRangeDeliveryType(1024 * 1024),
            expectedValidator: { kind: "etag", value: "\"abc\"" },
            chunkSize: 1024 * 1024,
          },
          overlay: {
            fileName: "remote.overlay.aerospar",
            diskSizeBytes: 20 * 1024 * 1024,
            blockSizeBytes: 1024 * 1024,
          },
          cache: {
            fileName: "remote.cache.aerospar",
            cacheLimitBytes: DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES,
          },
        },
      },
    ],
  };
}

describe("runtime disk snapshot payload", () => {
  it("serializes and deserializes (roundtrip)", () => {
    const snapshot = sampleSnapshot();

    const bytes = serializeRuntimeDiskSnapshot(snapshot);
    const decoded = deserializeRuntimeDiskSnapshot(bytes);
    // The deserializer returns plain record objects that are safe against prototype pollution
    // (e.g. null-prototype). Compare via JSON so prototype differences don't affect equality.
    expect(JSON.parse(JSON.stringify(decoded))).toEqual(snapshot);

    // Sanity: payload must not contain embedded URLs/tokens.
    const json = new TextDecoder().decode(bytes);
    expect(json).not.toContain("http");
    expect(json).not.toContain("token");
    expect(json).not.toContain("cookie");
  });

  it("rejects oversized payloads", () => {
    const bytes = new Uint8Array(1024 * 1024 + 1);
    expect(() => deserializeRuntimeDiskSnapshot(bytes)).toThrow(/too large/);
  });

  it("rejects wrong version", () => {
    const snapshot = { ...sampleSnapshot(), version: 2 };
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/Unsupported disk snapshot version/);
  });

  it("rejects missing disks array", () => {
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson({ version: 1, nextHandle: 1 }))).toThrow(/disks/);
  });

  it("does not accept required fields inherited from Object.prototype", () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "version");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }
    try {
      Object.defineProperty(Object.prototype, "version", { value: 1, configurable: true });
      // Missing `version` must not be satisfied by prototype pollution.
      expect(() => deserializeRuntimeDiskSnapshot(encodeJson({ nextHandle: 1, disks: [] }))).toThrow(/Unsupported disk snapshot version/);
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "version", existing);
      else Reflect.deleteProperty(Object.prototype, "version");
    }
  });

  it("does not observe optional fields inherited from Object.prototype", () => {
    const existing = Object.getOwnPropertyDescriptor(Object.prototype, "leaseEndpoint");
    if (existing && existing.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }
    try {
      Object.defineProperty(Object.prototype, "leaseEndpoint", { value: "/evil", configurable: true });
      const decoded = deserializeRuntimeDiskSnapshot(serializeRuntimeDiskSnapshot(sampleSnapshot()));
      const remote = decoded.disks[1]?.backend;
      expect(remote?.kind).toBe("remote");
      if (!remote || remote.kind !== "remote") {
        throw new Error("Expected decoded snapshot to contain a remote disk at index 1");
      }
      expect(remote.base.leaseEndpoint).toBeUndefined();
    } finally {
      if (existing) Object.defineProperty(Object.prototype, "leaseEndpoint", existing);
      else Reflect.deleteProperty(Object.prototype, "leaseEndpoint");
    }
  });

  it("defaults remote cacheLimitBytes when missing (backward compatibility)", () => {
    const baseSnapshot = sampleSnapshot();
    const disks = baseSnapshot.disks.map((disk, idx) => {
      if (idx !== 1) return disk;
      if (disk.backend.kind !== "remote") return disk;
      const { cacheLimitBytes: _cacheLimitBytes, ...cacheWithoutLimit } = disk.backend.cache;
      return { ...disk, backend: { ...disk.backend, cache: cacheWithoutLimit } };
    });
    const snapshot = { ...baseSnapshot, disks };

    const decoded = deserializeRuntimeDiskSnapshot(encodeJson(snapshot));
    expect(decoded.disks[1]?.backend).toMatchObject({
      kind: "remote",
      cache: { cacheLimitBytes: DEFAULT_REMOTE_DISK_CACHE_LIMIT_BYTES },
    });
  });

  it("rejects disk entry with non-integer handle", () => {
    const snapshot = sampleSnapshot();
    snapshot.disks[0].handle = 1.5;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/handle/);
  });

  it("rejects unsupported sectorSize", () => {
    const snapshot = sampleSnapshot();
    snapshot.disks[0].sectorSize = 123;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/sectorSize/);
  });

  it("rejects remote leaseEndpoint with http://", () => {
    const snapshot = sampleSnapshot();
    const backend = snapshot.disks[1]?.backend;
    if (!backend || backend.kind !== "remote") {
      throw new Error("Expected sample snapshot disk[1] to be remote");
    }
    backend.base.leaseEndpoint = "http://evil.example/lease";
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/leaseEndpoint/);
  });

  it("rejects remote chunkSize not multiple of 512", () => {
    const snapshot = sampleSnapshot();
    const backend = snapshot.disks[1]?.backend;
    if (!backend || backend.kind !== "remote") {
      throw new Error("Expected sample snapshot disk[1] to be remote");
    }
    backend.base.chunkSize = 1000;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/chunkSize/);
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/multiple of 512/);
  });

  it("rejects overlay blockSizeBytes that is not a power of two", () => {
    const snapshot = sampleSnapshot();
    const backend = snapshot.disks[0]?.backend;
    if (!backend || backend.kind !== "local") {
      throw new Error("Expected sample snapshot disk[0] to be local");
    }
    if (!backend.overlay) {
      throw new Error("Expected sample snapshot disk[0] to include an overlay");
    }
    backend.overlay.blockSizeBytes = 3 * 512;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/blockSizeBytes/);
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/power of two/);
  });

  it("rejects excessively long strings", () => {
    const snapshot = sampleSnapshot();
    const backend = snapshot.disks[0]?.backend;
    if (!backend || backend.kind !== "local") {
      throw new Error("Expected sample snapshot disk[0] to be local");
    }
    backend.key = "a".repeat(64 * 1024 + 1);
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/string too long/);
  });

  it("invalidates remote cache bindings on mismatch", () => {
    const expected: RemoteDiskBaseSnapshot = {
      imageId: "win7",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(1024),
      expectedValidator: { kind: "etag", value: "\"abc\"" },
      chunkSize: 1024,
    };

    const okBinding: RemoteCacheBinding = { version: 1, base: { ...expected } };
    expect(shouldInvalidateRemoteCache(expected, okBinding)).toBe(false);

    expect(
      shouldInvalidateRemoteCache(expected, {
        version: 1,
        base: { ...expected, imageId: "win7-other" },
      }),
    ).toBe(true);

    expect(
      shouldInvalidateRemoteCache(expected, {
        version: 1,
        base: { ...expected, expectedValidator: { kind: "etag", value: "\"def\"" } },
      }),
    ).toBe(true);

    expect(shouldInvalidateRemoteCache(expected, null)).toBe(true);

    // Corrupt/untrusted bindings should fail closed and invalidate the cache (best-effort).
    expect(
      shouldInvalidateRemoteCache(expected, {
        version: 1,
        base: { ...expected, deliveryType: 123 },
      } as unknown as RemoteCacheBinding),
    ).toBe(true);
  });

  it("does not accept remote cache binding fields inherited from Object.prototype", () => {
    const expected: RemoteDiskBaseSnapshot = {
      imageId: "win7",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(1024),
      expectedValidator: { kind: "etag", value: "\"abc\"" },
      chunkSize: 1024,
    };

    const existingVersion = Object.getOwnPropertyDescriptor(Object.prototype, "version");
    const existingBase = Object.getOwnPropertyDescriptor(Object.prototype, "base");
    if (
      (existingVersion && existingVersion.configurable === false) ||
      (existingBase && existingBase.configurable === false)
    ) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }
    try {
      Object.defineProperty(Object.prototype, "version", { value: 1, configurable: true });
      Object.defineProperty(Object.prototype, "base", { value: { ...expected }, configurable: true });

      // Missing `version`/`base` must not be satisfied by prototype pollution.
      expect(shouldInvalidateRemoteCache(expected, {} as unknown as RemoteCacheBinding)).toBe(true);
    } finally {
      if (existingVersion) Object.defineProperty(Object.prototype, "version", existingVersion);
      else Reflect.deleteProperty(Object.prototype, "version");
      if (existingBase) Object.defineProperty(Object.prototype, "base", existingBase);
      else Reflect.deleteProperty(Object.prototype, "base");
    }
  });

  it("does not invalidate remote overlays when binding is missing, but does when base identity changes", () => {
    const expected: RemoteDiskBaseSnapshot = {
      imageId: "win7",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(1024),
      expectedValidator: { kind: "etag", value: "\"abc\"" },
      chunkSize: 1024,
    };

    // Missing binding should keep the overlay (avoid data loss).
    expect(shouldInvalidateRemoteOverlay(expected, null)).toBe(false);

    const okBinding: RemoteCacheBinding = { version: 1, base: { ...expected } };
    expect(shouldInvalidateRemoteOverlay(expected, okBinding)).toBe(false);

    // Changing only chunk size should not invalidate overlay (cache tuning parameter).
    expect(
      shouldInvalidateRemoteOverlay(
        { ...expected, chunkSize: 2048, deliveryType: remoteRangeDeliveryType(2048) },
        okBinding,
      ),
    ).toBe(false);

    // Corrupt/untrusted bindings should not crash and should keep overlays (conservative).
    expect(
      shouldInvalidateRemoteOverlay(expected, {
        version: 1,
        base: { ...expected, deliveryType: 123 },
      } as unknown as RemoteCacheBinding),
    ).toBe(false);

    // Missing/invalid expectedValidator info is not positive evidence of mismatch; keep overlay.
    expect(
      shouldInvalidateRemoteOverlay(expected, {
        version: 1,
        base: { imageId: expected.imageId, version: expected.version, deliveryType: expected.deliveryType, chunkSize: expected.chunkSize },
      } as unknown as RemoteCacheBinding),
    ).toBe(false);

    expect(
      shouldInvalidateRemoteOverlay(expected, {
        version: 1,
        base: { ...expected, version: "v2" },
      }),
    ).toBe(true);
  });

  it("does not accept remote overlay binding fields inherited from Object.prototype", () => {
    const expected: RemoteDiskBaseSnapshot = {
      imageId: "win7",
      version: "v1",
      deliveryType: remoteRangeDeliveryType(1024),
      expectedValidator: { kind: "etag", value: "\"abc\"" },
      chunkSize: 1024,
    };

    const existingVersion = Object.getOwnPropertyDescriptor(Object.prototype, "version");
    const existingBase = Object.getOwnPropertyDescriptor(Object.prototype, "base");
    if (
      (existingVersion && existingVersion.configurable === false) ||
      (existingBase && existingBase.configurable === false)
    ) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }
    try {
      Object.defineProperty(Object.prototype, "version", { value: 1, configurable: true });
      Object.defineProperty(Object.prototype, "base", {
        value: { imageId: "evil", version: expected.version, deliveryType: expected.deliveryType, chunkSize: expected.chunkSize },
        configurable: true,
      });

      // Missing `version`/`base` must not be satisfied by prototype pollution. If they are, this would
      // cause overlay invalidation (data loss).
      expect(shouldInvalidateRemoteOverlay(expected, {} as unknown as RemoteCacheBinding)).toBe(false);
    } finally {
      if (existingVersion) Object.defineProperty(Object.prototype, "version", existingVersion);
      else Reflect.deleteProperty(Object.prototype, "version");
      if (existingBase) Object.defineProperty(Object.prototype, "base", existingBase);
      else Reflect.deleteProperty(Object.prototype, "base");
    }
  });
});
