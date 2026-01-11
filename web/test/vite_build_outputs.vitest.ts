import { execFileSync } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import { mkdtemp } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

describe("web Vite build outputs", () => {
  it("emits standalone HTML pages into dist", async () => {
    const webDir = fileURLToPath(new URL("..", import.meta.url));
    const viteBin = path.join(webDir, "..", "node_modules", "vite", "bin", "vite.js");
    const outDir = await mkdtemp(path.join(os.tmpdir(), "aero-web-dist-"));

    try {
      execFileSync(process.execPath, [viteBin, "build", "--config", path.join(webDir, "vite.config.ts"), "--outDir", outDir], {
        cwd: webDir,
        stdio: "inherit",
      });

      expect(existsSync(path.join(outDir, "webusb_diagnostics.html"))).toBe(true);
      expect(existsSync(path.join(outDir, "webgl2_fallback_demo.html"))).toBe(true);
    } finally {
      rmSync(outDir, { recursive: true, force: true });
    }
  });
});

