import { afterEach, describe, expect, it } from "vitest";

import { emptyState, opfsReadState, opfsWriteState } from "./metadata";
import { installMemoryOpfs, MemoryDirectoryHandle, MemoryFileHandle } from "../test_utils/memory_opfs";

let restoreOpfs: (() => void) | null = null;

afterEach(() => {
  restoreOpfs?.();
  restoreOpfs = null;
});

describe("opfsReadState", () => {
  it("treats oversized metadata.json files as corrupt without attempting to read them", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    // Pre-create the expected path and inject a fake oversized File-like object.
    const aeroDir = await root.getDirectoryHandle("aero", { create: true });
    const disksDir = await aeroDir.getDirectoryHandle("disks", { create: true });
    const handle = await disksDir.getFileHandle("metadata.json", { create: true });

    (handle as unknown as { getFile: () => Promise<unknown> }).getFile = async () => ({
      size: 64 * 1024 * 1024 + 1,
      async text() {
        throw new Error("should not read oversized metadata.json");
      },
      async arrayBuffer() {
        throw new Error("should not read oversized metadata.json");
      },
    });

    const state = await opfsReadState();
    expect(state).toEqual(emptyState());
  });

  it("aborts failed metadata.json writes so the previous state is preserved", async () => {
    const root = new MemoryDirectoryHandle("root");
    restoreOpfs = installMemoryOpfs(root).restore;

    await opfsWriteState({
      version: 2,
      disks: {
        a: {
          source: "local",
          id: "a",
          name: "disk a",
          backend: "opfs",
          kind: "hdd",
          format: "raw",
          fileName: "a.img",
          sizeBytes: 512,
          createdAtMs: 0,
        },
      },
      mounts: {},
    });

    const originalCreateWritable = MemoryFileHandle.prototype.createWritable;
    (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = async function (
      this: MemoryFileHandle,
      options?: { keepExistingData?: boolean },
    ) {
      const keepExistingData = options?.keepExistingData === true;
      const inner = await originalCreateWritable.call(this, options);

      // Only intercept state writes and only when overwriting.
      if (this.name !== "metadata.json" || keepExistingData) return inner;

      // Simulate OPFS semantics where keepExistingData=false truncates the file immediately, so
      // a failed write can otherwise corrupt the file unless abort() restores the previous content.
      const prev = await this.getFile();
      const prevBytes = new Uint8Array(await prev.arrayBuffer());

      // Truncate immediately.
      (this as unknown as { data: Uint8Array }).data = new Uint8Array();

      return {
        write: async () => {
          throw new Error("write failed");
        },
        close: async () => {
          throw new Error("close should not be called");
        },
        abort: async () => {
          (this as unknown as { data: Uint8Array }).data = prevBytes;
        },
      };
    };

    try {
      await expect(
        opfsWriteState({
          version: 2,
          disks: {
            b: {
              source: "local",
              id: "b",
              name: "disk b",
              backend: "opfs",
              kind: "hdd",
              format: "raw",
              fileName: "b.img",
              sizeBytes: 512,
              createdAtMs: 0,
            },
          },
          mounts: {},
        }),
      ).rejects.toThrow("write failed");
    } finally {
      (MemoryFileHandle.prototype as unknown as { createWritable: unknown }).createWritable = originalCreateWritable;
    }

    const state = await opfsReadState();
    expect(state.disks?.a?.id).toBe("a");
  });
});
