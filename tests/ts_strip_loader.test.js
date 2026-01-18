import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const REGISTER_TS_STRIP_LOADER_URL = new URL("../scripts/register-ts-strip-loader.mjs", import.meta.url);
const entryPath = fileURLToPath(new URL("./fixtures/ts_strip_loader/entry.ts", import.meta.url));
const entryExtensionlessPath = fileURLToPath(new URL("./fixtures/ts_strip_loader/entry_extensionless.ts", import.meta.url));
const entryAssetUrlPath = fileURLToPath(new URL("./fixtures/ts_strip_loader/entry_asset_url.ts", import.meta.url));

function runStripTypes(entry, { cwd } = {}) {
  return execFileSync(process.execPath, ["--experimental-strip-types", "--import", REGISTER_TS_STRIP_LOADER_URL.href, entry], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    cwd,
  });
}

test("ts-strip-loader: remaps relative .js?query specifiers to .ts equivalents", () => {
  const stdout = runStripTypes(entryPath);

  assert.ok(stdout.includes("module.ts?worker&url"), `expected stdout to include module.ts?worker&url, got:\n${stdout}`);
});

test("ts-strip-loader: supports extensionless directory imports via index.ts fallback", () => {
  const stdout = runStripTypes(entryExtensionlessPath);
  assert.equal(stdout.trim(), "AERO_EXTLESS_OK");
});

test("ts-strip-loader: synthesizes a default-exported URL for ?url asset imports", () => {
  const stdout = runStripTypes(entryAssetUrlPath);
  assert.ok(stdout.includes("asset.dat"), `expected stdout to include asset.dat, got:\n${stdout}`);
  assert.ok(!stdout.includes("?url"), `expected synthesized URL to drop ?url query, got:\n${stdout}`);
});

test("ts-strip-loader: resolves ws to ws-shim when node_modules is unavailable", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-ws-shim-"));
  try {
    const entry = path.join(dir, "entry.mts");
    fs.writeFileSync(
      entry,
      'import { WebSocketServer } from "ws";\nconsole.log(typeof WebSocketServer);\n',
      "utf8",
    );

    const stdout = runStripTypes(entry, { cwd: dir });
    assert.equal(stdout.trim(), "function");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
