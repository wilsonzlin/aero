import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const registerPath = fileURLToPath(new URL("../scripts/register-ts-strip-loader.mjs", import.meta.url));
const entryPath = fileURLToPath(new URL("./fixtures/ts_strip_loader/entry.ts", import.meta.url));

test("ts-strip-loader: remaps relative .js?query specifiers to .ts equivalents", () => {
  const stdout = execFileSync(process.execPath, ["--experimental-strip-types", "--import", registerPath, entryPath], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

  assert.ok(stdout.includes("module.ts?worker&url"), `expected stdout to include module.ts?worker&url, got:\n${stdout}`);
});
