import { afterEach, describe, expect, it } from "vitest";

import { DiskManager } from "./disk_manager";

type FakeExportHandle = {
  stream: { pipeTo: (dest: any) => Promise<void>; cancel: (reason?: unknown) => Promise<void> };
  done: Promise<{ checksumCrc32: string }>;
  meta: unknown;
};

class FakeWritableFileStream {
  private readonly parts: Uint8Array[] = [];
  private aborted = false;

  constructor(
    private readonly file: FakeFileHandle,
    keepExistingData: boolean,
  ) {
    if (keepExistingData) {
      this.parts.push(this.file.data.slice());
    }
  }

  async truncate(size: number): Promise<void> {
    if (this.aborted) throw new Error("truncate after abort");
    if (size !== 0) throw new Error("test stream only supports truncate(0)");
    this.parts.length = 0;
  }

  async write(chunk: Uint8Array): Promise<void> {
    if (this.aborted) throw new Error("write after abort");
    this.parts.push(chunk.slice());
  }

  async close(): Promise<void> {
    if (this.aborted) return;
    const total = this.parts.reduce((sum, p) => sum + p.byteLength, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const part of this.parts) {
      out.set(part, off);
      off += part.byteLength;
    }
    this.file.data = out;
  }

  async abort(): Promise<void> {
    this.aborted = true;
  }
}

class FakeFileHandle {
  data = new Uint8Array([1, 2, 3, 4, 5, 6]);
  createWritableCalls: Array<unknown[] | null> = [];

  async createWritable(...args: any[]): Promise<FakeWritableFileStream> {
    this.createWritableCalls.push(args);
    if (args.length > 0) {
      // Simulate an implementation that rejects the options bag.
      throw new Error("synthetic createWritable options not supported");
    }
    // Simulate a problematic default where the writable stream keeps existing data unless
    // explicitly truncated.
    return new FakeWritableFileStream(this, /* keepExistingData */ true);
  }
}

// `showSaveFilePicker` is declared in lib.dom, but may not exist in the Node test runtime.
// Store/restore via `any` to avoid type conflicts.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let originalShowSaveFilePicker: any = undefined;
let hadShowSaveFilePicker = false;

afterEach(() => {
  const g = globalThis as unknown as { showSaveFilePicker?: unknown };
  if (hadShowSaveFilePicker) {
    g.showSaveFilePicker = originalShowSaveFilePicker;
  } else {
    Reflect.deleteProperty(g, "showSaveFilePicker");
  }
  originalShowSaveFilePicker = undefined;
  hadShowSaveFilePicker = false;
});

describe("DiskManager.exportDiskToFile", () => {
  it("truncates when createWritable options are unsupported (prevents trailing bytes)", async () => {
    const g = globalThis as unknown as { showSaveFilePicker?: unknown };
    originalShowSaveFilePicker = g.showSaveFilePicker;
    hadShowSaveFilePicker = Object.prototype.hasOwnProperty.call(g, "showSaveFilePicker");

    const pickerHandle = new FakeFileHandle();
    g.showSaveFilePicker = async () => pickerHandle as unknown as FileSystemFileHandle;

    // Construct a DiskManager with a dummy worker; we'll override exportDiskStream below so the
    // worker is never used.
    const worker = { postMessage() {}, terminate() {}, onmessage: null } as unknown as Worker;
    const manager = new DiskManager({ backend: "opfs", worker });
    try {
      const exportHandle: FakeExportHandle = {
        stream: {
          async pipeTo(dest: any) {
            await dest.write(new Uint8Array([9, 9]));
            await dest.close();
          },
          async cancel() {
            // ignore
          },
        },
        done: Promise.resolve({ checksumCrc32: "deadbeef" }),
        meta: { id: "d", name: "disk", format: "raw" },
      };

      (manager as unknown as { exportDiskStream: () => Promise<FakeExportHandle> }).exportDiskStream = async () => exportHandle;

      const res = await manager.exportDiskToFile("d", { suggestedName: "out.bin" });
      expect(res.fileName).toBe("out.bin");

      // Ensure we attempted the options form and then fell back.
      expect(pickerHandle.createWritableCalls.length).toBe(2);
      expect(pickerHandle.createWritableCalls[0]?.length).toBeGreaterThan(0);
      expect(pickerHandle.createWritableCalls[1]?.length).toBe(0);

      // The destination should be overwritten exactly (no trailing bytes from the original [1..6]).
      expect(Array.from(pickerHandle.data)).toEqual([9, 9]);
    } finally {
      manager.close();
    }
  });
});
