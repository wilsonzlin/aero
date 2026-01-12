import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";

function writeExecutable(filePath, contents) {
  fs.writeFileSync(filePath, contents, "utf8");
  fs.chmodSync(filePath, 0o755);
}

function rustcHostTarget() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const m = out.match(/^host:\s*(.+)\s*$/m);
  assert.ok(m, `failed to parse rustc host target from: ${out}`);
  return m[1].trim();
}

function cargoTargetRustflagsVar(target) {
  return `CARGO_TARGET_${target.toUpperCase().replace(/[-.]/g, "_")}_RUSTFLAGS`;
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
      const hostTargetVar = cargoTargetRustflagsVar(rustcHostTarget());

      writeExecutable(
        path.join(binDir, "cargo"),
        `#!/usr/bin/env bash
set -euo pipefail

out="\${AERO_TEST_OUTPUT:?}"
{
  echo "CARGO_BUILD_JOBS=\${CARGO_BUILD_JOBS:-}"
  echo "RUSTC_WORKER_THREADS=\${RUSTC_WORKER_THREADS:-}"
  echo "RAYON_NUM_THREADS=\${RAYON_NUM_THREADS:-}"
  var="\${AERO_TEST_HOST_RUSTFLAGS_VAR:-}"
  if [[ -n "\${var}" ]]; then
    echo "HOST_RUSTFLAGS=\${!var:-}"
  fi
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
          AERO_TEST_HOST_RUSTFLAGS_VAR: hostTargetVar,
          // Ensure we exercise the defaulting path.
          AERO_CARGO_BUILD_JOBS: null,
          CARGO_BUILD_JOBS: null,
          RUSTC_WORKER_THREADS: null,
          RAYON_NUM_THREADS: null,
          AERO_TOKIO_WORKER_THREADS: null,
          // Ensure we exercise the lld threads injection path (do not inherit from safe-run).
          [hostTargetVar]: null,
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
        HOST_RUSTFLAGS: process.platform === "linux" ? "-C link-arg=-Wl,--threads=1" : "",
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
      const hostTargetVar = cargoTargetRustflagsVar(rustcHostTarget());

      writeExecutable(
        path.join(binDir, "cargo"),
        `#!/usr/bin/env bash
set -euo pipefail

out="\${AERO_TEST_OUTPUT:?}"
{
  echo "CARGO_BUILD_JOBS=\${CARGO_BUILD_JOBS:-}"
  echo "RUSTC_WORKER_THREADS=\${RUSTC_WORKER_THREADS:-}"
  echo "RAYON_NUM_THREADS=\${RAYON_NUM_THREADS:-}"
  var="\${AERO_TEST_HOST_RUSTFLAGS_VAR:-}"
  if [[ -n "\${var}" ]]; then
    echo "HOST_RUSTFLAGS=\${!var:-}"
  fi
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
          AERO_TEST_HOST_RUSTFLAGS_VAR: hostTargetVar,
          AERO_CARGO_BUILD_JOBS: "2",
          CARGO_BUILD_JOBS: null,
          RUSTC_WORKER_THREADS: null,
          RAYON_NUM_THREADS: null,
          AERO_TOKIO_WORKER_THREADS: null,
          [hostTargetVar]: null,
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
        HOST_RUSTFLAGS: process.platform === "linux" ? "-C link-arg=-Wl,--threads=2" : "",
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
