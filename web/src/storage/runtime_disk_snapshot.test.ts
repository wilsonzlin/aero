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
    expect(decoded).toEqual(snapshot);

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
    const snapshot = sampleSnapshot() as any;
    snapshot.version = 2;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/Unsupported disk snapshot version/);
  });

  it("rejects missing disks array", () => {
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson({ version: 1, nextHandle: 1 }))).toThrow(/disks/);
  });

  it("rejects disk entry with non-integer handle", () => {
    const snapshot = sampleSnapshot() as any;
    snapshot.disks[0].handle = 1.5;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/handle/);
  });

  it("rejects unsupported sectorSize", () => {
    const snapshot = sampleSnapshot() as any;
    snapshot.disks[0].sectorSize = 123;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/sectorSize/);
  });

  it("rejects remote leaseEndpoint with http://", () => {
    const snapshot = sampleSnapshot() as any;
    snapshot.disks[1].backend.base.leaseEndpoint = "http://evil.example/lease";
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/leaseEndpoint/);
  });

  it("rejects remote chunkSize not multiple of 512", () => {
    const snapshot = sampleSnapshot() as any;
    snapshot.disks[1].backend.base.chunkSize = 1000;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/chunkSize/);
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/multiple of 512/);
  });

  it("rejects overlay blockSizeBytes that is not a power of two", () => {
    const snapshot = sampleSnapshot() as any;
    snapshot.disks[0].backend.overlay.blockSizeBytes = 3 * 512;
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/blockSizeBytes/);
    expect(() => deserializeRuntimeDiskSnapshot(encodeJson(snapshot))).toThrow(/power of two/);
  });

  it("rejects excessively long strings", () => {
    const snapshot = sampleSnapshot() as any;
    snapshot.disks[0].backend.key = "a".repeat(64 * 1024 + 1);
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

    expect(
      shouldInvalidateRemoteOverlay(expected, {
        version: 1,
        base: { ...expected, version: "v2" },
      }),
    ).toBe(true);
  });
});
