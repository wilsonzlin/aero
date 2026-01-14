import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const harnessPath = path.join(
  repoRoot,
  "drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py",
);

function detectPython() {
  const candidates = process.platform === "win32" ? ["python", "python3"] : ["python3", "python"];
  for (const cmd of candidates) {
    const res = spawnSync(cmd, ["--version"], { encoding: "utf8" });
    if (!res.error && res.status === 0) return cmd;
  }
  return null;
}

const python = detectPython();

test(
  "Win7 host harness --help keeps --virtio-transitional description in sync with virtio-input behavior",
  { skip: python === null },
  () => {
    const res = spawnSync(python, [harnessPath, "--help"], {
      cwd: repoRoot,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(
      res.status,
      0,
      `expected exit=0, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`,
    );

    // argparse typically prints help to stdout, but combine streams to be safe.
    const help = `${res.stdout}${res.stderr}`;

    assert.ok(
      help.includes("virtio-keyboard-pci/virtio-mouse-pci"),
      `expected --help to mention virtio-keyboard-pci/virtio-mouse-pci\n\n${help}`,
    );
    assert.match(
      help,
      /guest virtio-input selftest will likely FAIL/i,
      `expected --help to warn about virtio-input selftest failure\n\n${help}`,
    );
    assert.doesNotMatch(
      help,
      /does not attach\s+virtio-input/i,
      `--help contains stale wording suggesting virtio-input is never attached in transitional mode\n\n${help}`,
    );

    // Ensure recently added gating knobs remain discoverable via --help (prevents accidental removal).
    assert.ok(
      help.includes("--with-snd-buffer-limits"),
      `expected --help to mention --with-snd-buffer-limits\n\n${help}`,
    );
  },
);
