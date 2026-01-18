import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

async function readRepoFile(relPath) {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const root = path.resolve(here, "..");
  const abs = path.resolve(root, relPath);
  return await readFile(abs, "utf8");
}

function assertIncludes(text, needle, message) {
  assert.ok(text.includes(needle), message);
}

test("d.ts typing: safe helpers use PropertyKey for property/method keys", async () => {
  const safeProps = await readRepoFile("src/safe_props.d.ts");
  assertIncludes(
    safeProps,
    "export function tryGetProp(obj: unknown, key: PropertyKey): unknown | undefined;",
    "Expected safe_props.d.ts to type key as PropertyKey for tryGetProp",
  );
  assertIncludes(
    safeProps,
    "export function tryGetStringProp(obj: unknown, key: PropertyKey): string | undefined;",
    "Expected safe_props.d.ts to type key as PropertyKey for tryGetStringProp",
  );
  assertIncludes(
    safeProps,
    "export function tryGetNumberProp(obj: unknown, key: PropertyKey): number | undefined;",
    "Expected safe_props.d.ts to type key as PropertyKey for tryGetNumberProp",
  );

  const socketSafe = await readRepoFile("src/socket_safe.d.ts");
  assertIncludes(
    socketSafe,
    "export function tryGetMethodBestEffort(obj: unknown, key: PropertyKey): ((this: unknown, ...args: unknown[]) => unknown) | null;",
    "Expected socket_safe.d.ts to type key as PropertyKey for tryGetMethodBestEffort",
  );
  assertIncludes(
    socketSafe,
    "export function callMethodBestEffort(obj: unknown, key: PropertyKey, ...args: unknown[]): boolean;",
    "Expected socket_safe.d.ts to type key as PropertyKey for callMethodBestEffort",
  );
  assertIncludes(
    socketSafe,
    "export function callMethodCaptureErrorBestEffort(obj: unknown, key: PropertyKey, ...args: unknown[]): unknown | null;",
    "Expected socket_safe.d.ts to type key as PropertyKey for callMethodCaptureErrorBestEffort",
  );
});

