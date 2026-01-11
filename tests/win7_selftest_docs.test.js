import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

const docs = [
  "drivers/windows7/tests/README.md",
  "drivers/windows7/tests/guest-selftest/README.md",
  "drivers/windows7/virtio-snd/tests/qemu/README.md",
];

const forbiddenPatterns = [
  {
    re: /AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|SKIP\|/g,
    why: "virtio-snd marker is machine-friendly and does not include a SKIP reason code (see log text / capture marker instead).",
  },
  {
    re: /AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|PASS\|/g,
    why: "virtio-snd PASS marker does not include extra fields (only capture markers do).",
  },
  {
    re: /AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|FAIL\|device_missing/g,
    why: "device_missing is reported on the virtio-snd-capture marker; the virtio-snd marker uses plain FAIL.",
  },
  {
    re: /AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|FAIL\|topology_interface_missing/g,
    why: "topology_interface_missing is reported on the virtio-snd-capture marker; the virtio-snd marker uses plain FAIL.",
  },
  {
    re: /AERO_VIRTIO_SELFTEST\|TEST\|virtio-(?:blk|net)\|(PASS|FAIL)\|/g,
    why: "virtio-blk and virtio-net markers are PASS/FAIL without extra fields (other tests may include extra details).",
  },
];

test("Windows 7 virtio selftest docs avoid stale marker formats", () => {
  for (const relPath of docs) {
    const absPath = path.join(repoRoot, relPath);
    const contents = fs.readFileSync(absPath, "utf8");
    for (const { re, why } of forbiddenPatterns) {
      const matches = contents.match(re);
      assert.equal(
        matches,
        null,
        `${relPath} contains forbidden marker format '${matches?.[0] ?? re}'.\n${why}`,
      );
    }
  }
});

