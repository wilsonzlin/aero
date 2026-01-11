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

describe("runtime disk snapshot payload", () => {
  it("serializes and deserializes (roundtrip)", () => {
    const snapshot: RuntimeDiskSnapshot = {
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
              deliveryType: "range",
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

    const bytes = serializeRuntimeDiskSnapshot(snapshot);
    const decoded = deserializeRuntimeDiskSnapshot(bytes);
    expect(decoded).toEqual(snapshot);

    // Sanity: payload must not contain embedded URLs/tokens.
    const json = new TextDecoder().decode(bytes);
    expect(json).not.toContain("http");
    expect(json).not.toContain("token");
    expect(json).not.toContain("cookie");
  });

  it("invalidates remote cache bindings on mismatch", () => {
    const expected: RemoteDiskBaseSnapshot = {
      imageId: "win7",
      version: "v1",
      deliveryType: "range",
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
      deliveryType: "range",
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
        { ...expected, chunkSize: 2048 },
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
