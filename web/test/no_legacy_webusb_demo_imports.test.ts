import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

function collectSourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, ent.name);
    if (ent.isDirectory()) {
      out.push(...collectSourceFiles(full));
      continue;
    }
    if (!ent.isFile()) continue;
    if (full.endsWith(".d.ts")) continue;
    if (!/\.(ts|tsx|mts|cts)$/.test(full)) continue;
    out.push(full);
  }
  return out;
}

test("web/src does not import the legacy repo-root WebUSB demo stack (src/platform/webusb_* or src/platform/legacy/webusb_*)", () => {
  const webSrcDir = fileURLToPath(new URL("../src", import.meta.url));
  const files = collectSourceFiles(webSrcDir);

  const offenders: string[] = [];
  const forbidden =
    /(?:from\s+|import\s*\()\s*["'][^"']*src\/platform\/(?:legacy\/)?webusb_(?:broker|client|protocol)(?:\.ts)?["']/;

  for (const file of files) {
    const contents = fs.readFileSync(file, "utf8");
    const lines = contents.split(/\r?\n/);
    for (let i = 0; i < lines.length; i += 1) {
      const line = lines[i];
      if (!forbidden.test(line)) continue;
      offenders.push(`${path.relative(process.cwd(), file)}:${i + 1}: ${line.trim()}`);
    }
  }

  assert.deepEqual(
    offenders,
    [],
    [
      "Found forbidden imports from the legacy repo-root WebUSB demo stack.",
      "",
      "The canonical WebUSB passthrough implementation lives in `web/src/usb/*` + `crates/aero-usb` (ADR 0015).",
      "The legacy demo RPC stack is quarantined under `src/platform/legacy/*` and must not be used by `web/src/**`.",
      "Do NOT grow a second wire contract under `src/platform/webusb_*`.",
      "",
      ...offenders,
    ].join("\n"),
  );
});
