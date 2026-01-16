import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

function gitGrepFiles(pattern: string): string[] {
  try {
    const out = execFileSync("git", ["grep", "-l", pattern], { encoding: "utf8" });
    return out.split(/\r?\n/).filter(Boolean);
  } catch (err) {
    // `git grep` returns exit code 1 when there are no matches.
    if (typeof err === "object" && err !== null && "status" in err && (err as { status?: unknown }).status === 1) {
      return [];
    }
    throw err;
  }
}

function isSourceFile(file: string): boolean {
  if (file.endsWith(".d.ts")) return false;
  return /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/.test(file);
}

test("no non-legacy code imports the legacy repo-root WebUSB demo modules (src/platform/legacy/webusb_*)", () => {
  // Only scan files that even mention the legacy modules; keeps the check cheap.
  const candidates = gitGrepFiles("legacy/webusb_").filter(isSourceFile);

  const offenders: string[] = [];
  const forbiddenImport =
    /\b(?:from\s+|import\s*\(\s*|import\s+|require\s*\(\s*)["'][^"']*legacy\/webusb_(?:broker|client|protocol)(?:\.ts)?["']/g;

  for (const file of candidates) {
    // Imports *within* the quarantined legacy dir are OK.
    if (file.startsWith("src/platform/legacy/")) continue;

    const contents = fs.readFileSync(file, "utf8");
    forbiddenImport.lastIndex = 0;
    for (;;) {
      const match = forbiddenImport.exec(contents);
      if (!match) break;

      const idx = match.index;
      const before = contents.slice(0, idx);
      const lineNo = before.split(/\r?\n/).length;
      const lineStart = Math.max(0, before.lastIndexOf("\n") + 1);
      const lineEnd = contents.indexOf("\n", idx);
      const line = contents.slice(lineStart, lineEnd === -1 ? contents.length : lineEnd).trim();

      offenders.push(`${file}:${lineNo}: ${line}`);
    }
  }

  assert.deepEqual(
    offenders,
    [],
    [
      "Found forbidden imports from the legacy repo-root WebUSB demo stack.",
      "",
      "The canonical WebUSB passthrough implementation lives in `web/src/usb/*` + `crates/aero-usb` (ADR 0015).",
      "A legacy demo stack previously lived under `src/platform/legacy/webusb_*.ts` and has been removed.",
      "Do not reintroduce it (or any parallel WebUSB demo stack) as production code.",
      "",
      "If you need WebUSB guest USB passthrough: use `web/src/usb/*` (UsbHostAction/UsbHostCompletion).",
      "",
      ...offenders,
    ].join("\n"),
  );
});
