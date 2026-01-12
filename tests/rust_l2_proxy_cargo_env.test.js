import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

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
  "tools/rust_l2_proxy.js defaults Cargo + rustc/rayon thread env vars for spawned cargo builds",
  { skip: process.platform === "win32" },
  async () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-rust-l2-proxy-env-default-"));
    try {
      const binDir = path.join(tmpRoot, "bin");
      fs.mkdirSync(binDir, { recursive: true });
      const targetDir = path.join(tmpRoot, "target");
      const outputPath = path.join(tmpRoot, "cargo-env.txt");
      const proxyEnvPath = path.join(tmpRoot, "proxy-env.txt");

      writeExecutable(
        path.join(binDir, "cargo"),
        `#!/usr/bin/env bash
set -euo pipefail

out="\${AERO_TEST_OUTPUT:?}"
{
  echo "CARGO_BUILD_JOBS=\${CARGO_BUILD_JOBS:-}"
  echo "RUSTC_WORKER_THREADS=\${RUSTC_WORKER_THREADS:-}"
  echo "RAYON_NUM_THREADS=\${RAYON_NUM_THREADS:-}"
} > "\${out}"

mkdir -p "\${CARGO_TARGET_DIR:?}/debug"
bin="\${CARGO_TARGET_DIR}/debug/aero-l2-proxy"
cat > "\${bin}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
out="\${AERO_TEST_PROXY_ENV_OUTPUT:?}"
echo "AERO_TOKIO_WORKER_THREADS=\${AERO_TOKIO_WORKER_THREADS:-}" > "\${out}"
echo "aero-l2-proxy listening on http://127.0.0.1:12345"
while true; do sleep 1; done
EOF
chmod +x "\${bin}"
exit 0
`,
      );

      await withEnv(
        {
          PATH: `${binDir}${path.delimiter}${process.env.PATH ?? ""}`,
          CARGO_TARGET_DIR: targetDir,
          AERO_TEST_OUTPUT: outputPath,
          AERO_TEST_PROXY_ENV_OUTPUT: proxyEnvPath,
          // Ensure we exercise the defaulting path.
          AERO_CARGO_BUILD_JOBS: null,
          CARGO_BUILD_JOBS: null,
          RUSTC_WORKER_THREADS: null,
          RAYON_NUM_THREADS: null,
          AERO_TOKIO_WORKER_THREADS: null,
        },
        async () => {
          const moduleUrl = new URL(
            `../tools/rust_l2_proxy.js?test=${encodeURIComponent(`default-${Date.now()}`)}`,
            import.meta.url,
          );
          const { startRustL2Proxy } = await import(moduleUrl.href);
          const proxy = await startRustL2Proxy();
          await proxy.close();
        },
      );

      const seen = parseKeyValues(fs.readFileSync(outputPath, "utf8"));
      assert.deepEqual(seen, {
        CARGO_BUILD_JOBS: "1",
        RUSTC_WORKER_THREADS: "1",
        RAYON_NUM_THREADS: "1",
      });

      const proxyEnv = parseKeyValues(fs.readFileSync(proxyEnvPath, "utf8"));
      assert.deepEqual(proxyEnv, {
        AERO_TOKIO_WORKER_THREADS: "1",
      });
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  },
);

test(
  "tools/rust_l2_proxy.js respects AERO_CARGO_BUILD_JOBS when spawning cargo builds",
  { skip: process.platform === "win32" },
  async () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-rust-l2-proxy-env-override-"));
    try {
      const binDir = path.join(tmpRoot, "bin");
      fs.mkdirSync(binDir, { recursive: true });
      const targetDir = path.join(tmpRoot, "target");
      const outputPath = path.join(tmpRoot, "cargo-env.txt");
      const proxyEnvPath = path.join(tmpRoot, "proxy-env.txt");

      writeExecutable(
        path.join(binDir, "cargo"),
        `#!/usr/bin/env bash
set -euo pipefail

out="\${AERO_TEST_OUTPUT:?}"
{
  echo "CARGO_BUILD_JOBS=\${CARGO_BUILD_JOBS:-}"
  echo "RUSTC_WORKER_THREADS=\${RUSTC_WORKER_THREADS:-}"
  echo "RAYON_NUM_THREADS=\${RAYON_NUM_THREADS:-}"
} > "\${out}"

mkdir -p "\${CARGO_TARGET_DIR:?}/debug"
bin="\${CARGO_TARGET_DIR}/debug/aero-l2-proxy"
cat > "\${bin}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
out="\${AERO_TEST_PROXY_ENV_OUTPUT:?}"
echo "AERO_TOKIO_WORKER_THREADS=\${AERO_TOKIO_WORKER_THREADS:-}" > "\${out}"
echo "aero-l2-proxy listening on http://127.0.0.1:12346"
while true; do sleep 1; done
EOF
chmod +x "\${bin}"
exit 0
`,
      );

      await withEnv(
        {
          PATH: `${binDir}${path.delimiter}${process.env.PATH ?? ""}`,
          CARGO_TARGET_DIR: targetDir,
          AERO_TEST_OUTPUT: outputPath,
          AERO_TEST_PROXY_ENV_OUTPUT: proxyEnvPath,
          AERO_CARGO_BUILD_JOBS: "2",
          CARGO_BUILD_JOBS: null,
          RUSTC_WORKER_THREADS: null,
          RAYON_NUM_THREADS: null,
          AERO_TOKIO_WORKER_THREADS: null,
        },
        async () => {
          const moduleUrl = new URL(
            `../tools/rust_l2_proxy.js?test=${encodeURIComponent(`override-${Date.now()}`)}`,
            import.meta.url,
          );
          const { startRustL2Proxy } = await import(moduleUrl.href);
          const proxy = await startRustL2Proxy();
          await proxy.close();
        },
      );

      const seen = parseKeyValues(fs.readFileSync(outputPath, "utf8"));
      assert.deepEqual(seen, {
        CARGO_BUILD_JOBS: "2",
        RUSTC_WORKER_THREADS: "2",
        RAYON_NUM_THREADS: "2",
      });

      const proxyEnv = parseKeyValues(fs.readFileSync(proxyEnvPath, "utf8"));
      assert.deepEqual(proxyEnv, {
        AERO_TOKIO_WORKER_THREADS: "2",
      });
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  },
);
