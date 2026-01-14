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

    it("passes through object-like disk metadata without deep validation", () => {
      const hdd = { some: "meta" };
      const cd = { other: "meta" };
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
