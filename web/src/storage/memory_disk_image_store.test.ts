import { describe, expect, it } from "vitest";

import { MemoryDiskImageStore } from "./memory_disk_image_store";
import { readBlobAsArrayBuffer } from "./disk_image_store";

describe("MemoryDiskImageStore", () => {
  it("imports, lists, exports, and deletes images", async () => {
    const store = new MemoryDiskImageStore();
    const bytes = new Uint8Array([1, 2, 3, 4, 5]);
    const file = new File([bytes], "disk.img", { type: "application/octet-stream" });

    const imported = await store.import(file);
    expect(imported.name).toBe("disk.img");
    expect(imported.size).toBe(bytes.byteLength);

    const listed = await store.list();
    expect(listed).toHaveLength(1);
    expect(listed[0].name).toBe("disk.img");

    const exported = await store.export("disk.img");
    expect(exported.size).toBe(bytes.byteLength);
    expect(new Uint8Array(await readBlobAsArrayBuffer(exported))).toEqual(bytes);

    await store.delete("disk.img");
    expect(await store.list()).toHaveLength(0);
  });

  it("handles name collisions by suffixing", async () => {
    const store = new MemoryDiskImageStore();
    const bytes = new Uint8Array([9, 9, 9]);

    const a = await store.import(new File([bytes], "win7.img"));
    const b = await store.import(new File([bytes], "win7.img"));
    const c = await store.import(new File([bytes], "win7.img"));

    expect(a.name).toBe("win7.img");
    expect(b.name).toBe("win7 (1).img");
    expect(c.name).toBe("win7 (2).img");

    const d = await store.import(new File([bytes], "ignored.img"), "custom.img");
    const e = await store.import(new File([bytes], "ignored.img"), "custom.img");
    expect(d.name).toBe("custom.img");
    expect(e.name).toBe("custom (1).img");
  });
});
