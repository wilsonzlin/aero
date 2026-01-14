import { describe, expect, it } from "vitest";

import { emptySetBootDisksMessage, normalizeSetBootDisksMessage } from "./boot_disks_protocol";

describe("runtime/boot_disks_protocol", () => {
  describe("emptySetBootDisksMessage", () => {
    it("returns a canonical empty message", () => {
      expect(emptySetBootDisksMessage()).toEqual({ type: "setBootDisks", mounts: {}, hdd: null, cd: null });
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
      expect(normalizeSetBootDisksMessage({ type: "setBootDisks" })).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
      });
      expect(normalizeSetBootDisksMessage({ type: "setBootDisks", mounts: 123, hdd: "nope", cd: false })).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
      });
    });

    it("sanitizes mount IDs to strings", () => {
      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: { hddId: 123, cdId: "cd0" },
          hdd: null,
          cd: null,
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: { cdId: "cd0" },
        hdd: null,
        cd: null,
      });
    });

    it("trims and drops empty mount ID strings", () => {
      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: { hddId: "   ", cdId: " cd0 " },
          hdd: null,
          cd: null,
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: { cdId: "cd0" },
        hdd: null,
        cd: null,
      });
    });

    it("ignores inherited mount IDs and disk metadata fields", () => {
      const mounts = Object.create({ hddId: "hdd0", cdId: "cd0" }) as Record<string, unknown>;
      // Own properties still work.
      mounts.cdId = "cd0";

      const inheritedDisk = Object.create({ source: "local", id: "hdd0", kind: "hdd" }) as Record<string, unknown>;
      // Provide only one required field as an own property; the rest are inherited => should be rejected.
      inheritedDisk.kind = "hdd";

      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts,
          hdd: inheritedDisk,
          cd: null,
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: { cdId: "cd0" },
        hdd: null,
        cd: null,
      });
    });

    it("ignores inherited top-level mounts/disk metadata fields", () => {
      const msg = Object.create({
        mounts: { hddId: "hdd0" },
        hdd: { source: "local", id: "hdd0", kind: "hdd" },
        bootDevice: "hdd",
      }) as Record<string, unknown>;
      // Only `type` is an own property; all other fields are inherited and must be ignored.
      msg.type = "setBootDisks";
      expect(normalizeSetBootDisksMessage(msg)).toEqual({ type: "setBootDisks", mounts: {}, hdd: null, cd: null });
    });

    it("accepts valid bootDevice values and drops invalid ones", () => {
      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: "cdrom",
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
        bootDevice: "cdrom",
      });

      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: "hdd",
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
        bootDevice: "hdd",
      });

      // Invalid bootDevice values are dropped rather than rejected outright.
      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: "floppy",
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
      });

      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: null,
          cd: null,
          bootDevice: 123,
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
      });
    });

    it("drops disk metadata objects that fail minimal shape checks", () => {
      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: { source: "local", id: "hdd0", kind: "cd" },
          cd: null,
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
      });

      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: { source: "local", id: "   ", kind: "hdd" },
          cd: null,
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
      });

      expect(
        normalizeSetBootDisksMessage({
          type: "setBootDisks",
          mounts: {},
          hdd: { source: "nope", id: "hdd0", kind: "hdd" },
          cd: null,
        }),
      ).toEqual({
        type: "setBootDisks",
        mounts: {},
        hdd: null,
        cd: null,
      });
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
      expect(msg?.mounts).toEqual({ hddId: "hdd0", cdId: "cd0" });
    });

  });
});
