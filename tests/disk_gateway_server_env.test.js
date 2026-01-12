import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

function writeExecutable(filePath, contents) {
  fs.writeFileSync(filePath, contents, "utf8");
  fs.chmodSync(filePath, 0o755);
}

function parseKeyValues(text) {
  const out = {};
  for (const line of text.trim().split(/\r?\n/)) {
    if (!line) continue;
    const idx = line.indexOf("=");
    if (idx === -1) continue;
    out[line.slice(0, idx)] = line.slice(idx + 1);
  }
  return out;
}

async function withEnv(overrides, fn) {
  const prev = { ...process.env };
  try {
    for (const k of Object.keys(process.env)) delete process.env[k];
    for (const [k, v] of Object.entries(prev)) process.env[k] = v;
    for (const [k, v] of Object.entries(overrides)) {
      if (v === null || v === undefined) delete process.env[k];
      else process.env[k] = String(v);
    }
    return await fn();
  } finally {
    for (const k of Object.keys(process.env)) delete process.env[k];
    for (const [k, v] of Object.entries(prev)) process.env[k] = v;
  }
}

test(
  "disk-streaming-browser-e2e harness defaults Cargo/rustc thread env vars for disk-gateway",
  { skip: process.platform === "win32" },
  async () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-disk-gateway-env-default-"));
    try {
      const binDir = path.join(tmpRoot, "bin");
      fs.mkdirSync(binDir, { recursive: true });

      const outputPath = path.join(tmpRoot, "cargo-env.txt");
      writeExecutable(
        path.join(binDir, "cargo"),
        `#!/usr/bin/env bash
set -euo pipefail

out="\${AERO_TEST_CARGO_ENV_OUT:?}"
{
  echo "CARGO_BUILD_JOBS=\${CARGO_BUILD_JOBS:-}"
  echo "RUSTC_WORKER_THREADS=\${RUSTC_WORKER_THREADS:-}"
  echo "RAYON_NUM_THREADS=\${RAYON_NUM_THREADS:-}"
} > "\${out}"

exec node -e '
  const http = require(\"node:http\");
  const bind = process.env.DISK_GATEWAY_BIND;
  if (!bind) throw new Error(\"missing DISK_GATEWAY_BIND\");
  const idx = bind.lastIndexOf(\":\");
  const host = bind.slice(0, idx);
  const port = Number(bind.slice(idx + 1));
  if (!Number.isSafeInteger(port)) throw new Error(\"invalid port in DISK_GATEWAY_BIND: \" + bind);
  const server = http.createServer((req, res) => {
    res.statusCode = 200;
    res.end();
  });
  server.listen(port, host);
'`,
      );

      const fixturePublic = path.join(tmpRoot, "public.img");
      const fixturePrivate = path.join(tmpRoot, "private.img");
      fs.writeFileSync(fixturePublic, Buffer.from([1, 2, 3]));
      fs.writeFileSync(fixturePrivate, Buffer.from([4, 5, 6]));

      const require = createRequire(import.meta.url);
      const { startDiskGatewayServer } = require("../tools/disk-streaming-browser-e2e/src/servers.js");

      await withEnv(
        {
          PATH: `${binDir}${path.delimiter}${process.env.PATH ?? ""}`,
          AERO_TEST_CARGO_ENV_OUT: outputPath,
          // Ensure we exercise the defaulting logic.
          AERO_CARGO_BUILD_JOBS: null,
          CARGO_BUILD_JOBS: null,
          RUSTC_WORKER_THREADS: null,
          RAYON_NUM_THREADS: null,
        },
        async () => {
          const server = await startDiskGatewayServer({
            appOrigin: "http://127.0.0.1:1",
            publicFixturePath: fixturePublic,
            privateFixturePath: fixturePrivate,
          });
          try {
            // No-op: startDiskGatewayServer already validated readiness.
          } finally {
            await server.close();
          }
        },
      );

      const seen = parseKeyValues(fs.readFileSync(outputPath, "utf8"));
      assert.deepEqual(seen, {
        CARGO_BUILD_JOBS: "1",
        RUSTC_WORKER_THREADS: "1",
        RAYON_NUM_THREADS: "1",
      });
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  },
);

test(
  "disk-streaming-browser-e2e harness respects AERO_CARGO_BUILD_JOBS when spawning disk-gateway",
  { skip: process.platform === "win32" },
  async () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-disk-gateway-env-override-"));
    try {
      const binDir = path.join(tmpRoot, "bin");
      fs.mkdirSync(binDir, { recursive: true });

      const outputPath = path.join(tmpRoot, "cargo-env.txt");
      writeExecutable(
        path.join(binDir, "cargo"),
        `#!/usr/bin/env bash
set -euo pipefail

out="\${AERO_TEST_CARGO_ENV_OUT:?}"
{
  echo "CARGO_BUILD_JOBS=\${CARGO_BUILD_JOBS:-}"
  echo "RUSTC_WORKER_THREADS=\${RUSTC_WORKER_THREADS:-}"
  echo "RAYON_NUM_THREADS=\${RAYON_NUM_THREADS:-}"
} > "\${out}"

exec node -e '
  const http = require(\"node:http\");
  const bind = process.env.DISK_GATEWAY_BIND;
  if (!bind) throw new Error(\"missing DISK_GATEWAY_BIND\");
  const idx = bind.lastIndexOf(\":\");
  const host = bind.slice(0, idx);
  const port = Number(bind.slice(idx + 1));
  if (!Number.isSafeInteger(port)) throw new Error(\"invalid port in DISK_GATEWAY_BIND: \" + bind);
  const server = http.createServer((req, res) => {
    res.statusCode = 200;
    res.end();
  });
  server.listen(port, host);
'`,
      );

      const fixturePublic = path.join(tmpRoot, "public.img");
      const fixturePrivate = path.join(tmpRoot, "private.img");
      fs.writeFileSync(fixturePublic, Buffer.from([1, 2, 3]));
      fs.writeFileSync(fixturePrivate, Buffer.from([4, 5, 6]));

      const require = createRequire(import.meta.url);
      const { startDiskGatewayServer } = require("../tools/disk-streaming-browser-e2e/src/servers.js");

      await withEnv(
        {
          PATH: `${binDir}${path.delimiter}${process.env.PATH ?? ""}`,
          AERO_TEST_CARGO_ENV_OUT: outputPath,
          AERO_CARGO_BUILD_JOBS: "2",
          CARGO_BUILD_JOBS: null,
          RUSTC_WORKER_THREADS: null,
          RAYON_NUM_THREADS: null,
        },
        async () => {
          const server = await startDiskGatewayServer({
            appOrigin: "http://127.0.0.1:1",
            publicFixturePath: fixturePublic,
            privateFixturePath: fixturePrivate,
          });
          try {
            // No-op: startDiskGatewayServer already validated readiness.
          } finally {
            await server.close();
          }
        },
      );

      const seen = parseKeyValues(fs.readFileSync(outputPath, "utf8"));
      assert.deepEqual(seen, {
        CARGO_BUILD_JOBS: "2",
        RUSTC_WORKER_THREADS: "2",
        RAYON_NUM_THREADS: "2",
      });
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  },
);
