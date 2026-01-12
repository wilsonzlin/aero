import { afterEach, describe, expect, it } from "vitest";

import { emptyState, opfsReadState } from "./metadata";
import { installMemoryOpfs, MemoryDirectoryHandle } from "../test_utils/memory_opfs";

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

    (handle as any).getFile = async () => ({
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
});

