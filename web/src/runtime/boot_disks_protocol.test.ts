import { describe, expect, it } from "vitest";

import { emptySetBootDisksMessage, normalizeSetBootDisksMessage } from "./boot_disks_protocol";

describe("runtime/boot_disks_protocol", () => {
  describe("emptySetBootDisksMessage", () => {
    it("returns a canonical empty message", () => {
      const msg = emptySetBootDisksMessage();
      expect(msg.type).toBe("setBootDisks");
      expect({ ...msg.mounts }).toEqual({});
      expect(msg.hdd).toBeNull();
      expect(msg.cd).toBeNull();
    });
  });

  describe("normalizeSetBootDisksMessage", () => {
    it("returns null for non-object inputs", () => {
      expect(normalizeSetBootDisksMessage(null)).toBeNull();
      expect(normalizeSetBootDisksMessage(undefined)).toBeNull();
      expect(normalizeSetBootDisksMessage(123)).toBeNull();
      expect(normalizeSetBootDisksMessage([])).toBeNull();
    });

    it("returns null for mismatched message types", () => {
      expect(normalizeSetBootDisksMessage({ type: "other" })).toBeNull();
    });

    it("requires type to be an own property", () => {
      expect(normalizeSetBootDisksMessage(Object.create({ type: "setBootDisks" }))).toBeNull();
    });

    it("normalizes missing/invalid fields to empty defaults", () => {
      {
        const msg = normalizeSetBootDisksMessage({ type: "setBootDisks" });
        expect(msg).not.toBeNull();
        expect(msg?.type).toBe("setBootDisks");
        expect({ ...(msg?.mounts ?? {}) }).toEqual({});
        expect(msg?.hdd).toBeNull();
        expect(msg?.cd).toBeNull();
      }
      {
        const msg = normalizeSetBootDisksMessage({ type: "setBootDisks", mounts: 123, hdd: "nope", cd: false });
        expect(msg).not.toBeNull();
        expect(msg?.type).toBe("setBootDisks");
        expect({ ...(msg?.mounts ?? {}) }).toEqual({});
        expect(msg?.hdd).toBeNull();
        expect(msg?.cd).toBeNull();
      }
    });

    it("sanitizes mount IDs to strings", () => {
      const msg = normalizeSetBootDisksMessage({
        type: "setBootDisks",
        mounts: { hddId: 123, cdId: "cd0" },
        hdd: null,
        cd: null,
      });
      expect(msg).not.toBeNull();
      expect(msg?.type).toBe("setBootDisks");
      expect({ ...(msg?.mounts ?? {}) }).toEqual({ cdId: "cd0" });
      expect(msg?.hdd).toBeNull();
      expect(msg?.cd).toBeNull();
    });

    it("trims and drops empty mount ID strings", () => {
      const msg = normalizeSetBootDisksMessage({
        type: "setBootDisks",
        mounts: { hddId: "   ", cdId: " cd0 " },
        hdd: null,
        cd: null,
      });
      expect(msg).not.toBeNull();
      expect(msg?.type).toBe("setBootDisks");
      expect({ ...(msg?.mounts ?? {}) }).toEqual({ cdId: "cd0" });
      expect(msg?.hdd).toBeNull();
      expect(msg?.cd).toBeNull();
    });

    it("ignores inherited mount IDs and disk metadata fields", () => {
      const mounts = Object.create({ hddId: "hdd0", cdId: "cd0" }) as Record<string, unknown>;
      // Own properties still work.
      mounts.cdId = "cd0";

      const inheritedDisk = Object.create({ source: "local", id: "hdd0", kind: "hdd" }) as Record<string, unknown>;
      // Provide only one required field as an own property; the rest are inherited => should be rejected.
      inheritedDisk.kind = "hdd";

      const msg = normalizeSetBootDisksMessage({
        type: "setBootDisks",
        mounts,
        hdd: inheritedDisk,
        cd: null,
      });
      expect(msg).not.toBeNull();
      expect(msg?.type).toBe("setBootDisks");
      expect({ ...(msg?.mounts ?? {}) }).toEqual({ cdId: "cd0" });
      expect(msg?.hdd).toBeNull();
      expect(msg?.cd).toBeNull();
    });

    it("ignores inherited top-level mounts/disk metadata fields", () => {
      const msg = Object.create({
        mounts: { hddId: "hdd0" },
        hdd: { source: "local", id: "hdd0", kind: "hdd" },
        bootDevice: "hdd",
      }) as Record<string, unknown>;
      // Only `type` is an own property; all other fields are inherited and must be ignored.
      msg.type = "setBootDisks";
      const normalized = normalizeSetBootDisksMessage(msg);
      expect(normalized).not.toBeNull();
      expect(normalized?.type).toBe("setBootDisks");
      expect({ ...(normalized?.mounts ?? {}) }).toEqual({});
      expect(normalized?.hdd).toBeNull();
      expect(normalized?.cd).toBeNull();
    });

    it("does not observe mount IDs inherited from Object.prototype", () => {
      const hddExisting = Object.getOwnPropertyDescriptor(Object.prototype, "hddId");
      const cdExisting = Object.getOwnPropertyDescriptor(Object.prototype, "cdId");
      if ((hddExisting && hddExisting.configurable === false) || (cdExisting && cdExisting.configurable === false)) {
        // Extremely unlikely, but avoid breaking the test environment.
        return;
      }

      try {
        Object.defineProperty(Object.prototype, "hddId", { value: "evil", configurable: true });
        Object.defineProperty(Object.prototype, "cdId", { value: "evil2", configurable: true });
        const msg = normalizeSetBootDisksMessage({ type: "setBootDisks" });
        expect(msg).not.toBeNull();
        expect((msg as any).mounts.hddId).toBeUndefined();
        expect((msg as any).mounts.cdId).toBeUndefined();
      } finally {
        if (hddExisting) Object.defineProperty(Object.prototype, "hddId", hddExisting);
        else delete (Object.prototype as any).hddId;
        if (cdExisting) Object.defineProperty(Object.prototype, "cdId", cdExisting);
        else delete (Object.prototype as any).cdId;
      }
    });

    it("accepts valid bootDevice values and drops invalid ones", () => {
      {
        const msg = normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: "cdrom",
        });
        expect(msg).not.toBeNull();
        expect(msg?.bootDevice).toBe("cdrom");
      }

      {
        const msg = normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: "hdd",
        });
        expect(msg).not.toBeNull();
        expect(msg?.bootDevice).toBe("hdd");
      }

      // Invalid bootDevice values are dropped rather than rejected outright.
      {
        const msg = normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: "floppy",
        });
        expect(msg).not.toBeNull();
        expect(msg?.bootDevice).toBeUndefined();
      }

      {
        const msg = normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: 123,
        });
        expect(msg).not.toBeNull();
        expect(msg?.bootDevice).toBeUndefined();
      }
    });

    it("drops disk metadata objects that fail minimal shape checks", () => {
      {
        const msg = normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: { source: "local", id: "hdd0", kind: "cd" },
          cd: null,
        });
        expect(msg).not.toBeNull();
        expect(msg?.hdd).toBeNull();
      }

      {
        const msg = normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: { source: "local", id: "   ", kind: "hdd" },
          cd: null,
        });
        expect(msg).not.toBeNull();
        expect(msg?.hdd).toBeNull();
      }

      {
        const msg = normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: { source: "nope", id: "hdd0", kind: "hdd" },
          cd: null,
        });
        expect(msg).not.toBeNull();
        expect(msg?.hdd).toBeNull();
      }
    });

    it("passes through object-like disk metadata without deep validation", () => {
      const hdd = { source: "local", id: "hdd0", kind: "hdd", some: "meta" };
      const cd = { source: "local", id: "cd0", kind: "cd", other: "meta" };
      const msg = normalizeSetBootDisksMessage({
        type: "setBootDisks",
        mounts: { hddId: "hdd0", cdId: "cd0" },
        hdd,
        cd,
      });
      expect(msg).not.toBeNull();
      expect(msg?.hdd).toBe(hdd);
      expect(msg?.cd).toBe(cd);
      expect({ ...(msg?.mounts ?? {}) }).toEqual({ hddId: "hdd0", cdId: "cd0" });
    });

  });
});
