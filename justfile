# Aero developer tasks
#
# This repo is evolving quickly. The recipes below are written to be:
# - "one command" for common workflows (setup/dev/build/test)
# - configurable via env vars (so we don't bake in assumptions about paths)
# - forgiving when parts of the repo (e.g. `web/`) don't exist yet
#
# Usage:
#   just setup
#   just dev
#   just build
#   just test
#
# Optional configuration:
#   WEB_DIR=web just dev
#   WEB_DIR=web WASM_CRATE_DIR=crates/aero-wasm just wasm-watch

set shell := ["bash", "-euo", "pipefail", "-c"]

WEB_DIR := env_var_or_default("WEB_DIR", "web")

[private]
_detect_wasm_crate_dir:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -n "${WASM_CRATE_DIR:-}" ]]; then
    if [[ -f "${WASM_CRATE_DIR}/Cargo.toml" ]]; then
      echo "${WASM_CRATE_DIR}"
      exit 0
    fi
    echo "error: WASM_CRATE_DIR is set to '${WASM_CRATE_DIR}', but Cargo.toml was not found there" >&2
    exit 1
  fi

  for candidate in \
    crates/wasm \
    crates/aero-wasm \
    crates/aero-ipc \
    wasm \
    rust/wasm
  do
    if [[ -f "${candidate}/Cargo.toml" ]]; then
      echo "${candidate}"
      exit 0
    fi
  done

  # Empty output signals "not found".
  echo ""

default:
  @just --list

setup:
  #!/usr/bin/env bash
  set -euo pipefail

  echo "==> Rust: installing wasm32 target"
  if ! command -v rustup >/dev/null; then
    echo "error: rustup is required (https://rustup.rs)" >&2
    exit 1
  fi
  rustup target add wasm32-unknown-unknown

  echo "==> Rust: installing formatter/linter components"
  rustup component add rustfmt clippy

  # Threaded/shared-memory WASM builds use `-Z build-std` and therefore need
  # nightly + rust-src. The web app's `npm run wasm:build` script depends on this.
  echo "==> Rust: ensuring nightly + rust-src are available (for threaded WASM builds)"
  rustup toolchain install nightly
  rustup target add wasm32-unknown-unknown --toolchain nightly
  rustup component add rust-src --toolchain nightly

  wasm_dir="$(just _detect_wasm_crate_dir)"
  if [[ -n "${wasm_dir}" ]]; then
    echo "==> Tooling: checking wasm build tools"
    if ! command -v wasm-pack >/dev/null; then
      cat >&2 <<'EOF'
error: wasm-pack is required to build the wasm package.

Install it with:
  cargo install wasm-pack
EOF
      exit 1
    fi
  else
    echo "==> Tooling: wasm crate not found yet; skipping wasm-pack checks"
  fi

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    echo "==> Web: installing JS dependencies (npm ci)"
    if ! command -v npm >/dev/null; then
      echo "error: npm is required to install web deps" >&2
      exit 1
    fi
    (cd "{{WEB_DIR}}" && npm ci)
  else
    echo "==> Web: '{{WEB_DIR}}/package.json' not found; skipping npm ci"
  fi

  echo "==> Setup complete"

wasm:
  #!/usr/bin/env bash
  set -euo pipefail

  # Prefer the web app's wasm build scripts, which produce both:
  # - web/src/wasm/pkg-single
  # - web/src/wasm/pkg-threaded (shared-memory)
  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['wasm:build']) ? 0 : 1)" >/dev/null 2>&1); then
      if ! command -v wasm-pack >/dev/null; then
        echo "error: wasm-pack not found; run 'just setup' (or 'cargo install wasm-pack')" >&2
        exit 1
      fi
      echo "==> Building WASM (single + threaded) via '{{WEB_DIR}}' npm scripts"
      (cd "{{WEB_DIR}}" && npm run wasm:build)
      exit 0
    fi
  fi

  # Fallback: build a single wasm-pack package directly from a detected crate.
  wasm_dir="$(just _detect_wasm_crate_dir)"
  if [[ -z "${wasm_dir}" ]]; then
    echo "==> wasm: no web wasm build scripts found and no Rust wasm crate detected (set WASM_CRATE_DIR=...); nothing to build"
    exit 0
  fi
  if ! command -v wasm-pack >/dev/null; then
    echo "error: wasm-pack not found; run 'just setup' (or 'cargo install wasm-pack')" >&2
    exit 1
  fi
  out_dir="${WASM_PKG_DIR:-${wasm_dir}/pkg}"
  mkdir -p "${out_dir}"
  echo "==> Building wasm package (fallback): ${wasm_dir} -> ${out_dir}"
  wasm-pack build "${wasm_dir}" --dev --target web --out-dir "${out_dir}"

wasm-single:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['wasm:build:single']) ? 0 : 1)" >/dev/null 2>&1); then
      if ! command -v wasm-pack >/dev/null; then
        echo "error: wasm-pack not found; run 'just setup' (or 'cargo install wasm-pack')" >&2
        exit 1
      fi
      echo "==> Building WASM (single) via '{{WEB_DIR}}' npm scripts"
      (cd "{{WEB_DIR}}" && npm run wasm:build:single)
      exit 0
    fi
  fi

  echo "==> wasm-single: web script not found; falling back to 'just wasm'"
  just wasm

wasm-threaded:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['wasm:build:threaded']) ? 0 : 1)" >/dev/null 2>&1); then
      if ! command -v wasm-pack >/dev/null; then
        echo "error: wasm-pack not found; run 'just setup' (or 'cargo install wasm-pack')" >&2
        exit 1
      fi
      echo "==> Building WASM (threaded/shared-memory) via '{{WEB_DIR}}' npm scripts"
      (cd "{{WEB_DIR}}" && npm run wasm:build:threaded)
      exit 0
    fi
  fi

  echo "error: wasm-threaded requires the web app's build script ({{WEB_DIR}}/package.json with wasm:build:threaded)" >&2
  exit 1

wasm-release:
  #!/usr/bin/env bash
  set -euo pipefail

  # The web wasm build scripts use `--release` already.
  just wasm

[private]
_maybe_run_web_script script:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ ! -f "{{WEB_DIR}}/package.json" ]]; then
    echo "==> Web: '{{WEB_DIR}}/package.json' not found; skipping '{{script}}'"
    exit 0
  fi

  # Only run if the script exists, otherwise keep things green for partial checkouts.
  if (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['{{script}}']) ? 0 : 1)" >/dev/null 2>&1); then
    (cd "{{WEB_DIR}}" && npm run "{{script}}")
  else
    echo "==> Web: no npm script named '{{script}}' (skipping)"
  fi

dev:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    echo "==> Building wasm (single + threaded)"
    just wasm

    cat <<'EOF'
==> Starting Vite dev server

Vite will print the local URL (usually http://localhost:5173).

Note: Aero relies on SharedArrayBuffer/WASM threads, which require cross-origin isolation
(COOP/COEP headers => `crossOriginIsolated === true`). If you see
`SharedArrayBuffer is not defined` or `crossOriginIsolated is false`, see the
Troubleshooting section in README.md.

Tip: For automatic wasm rebuilds while the dev server is running, use a second terminal:
  just wasm-watch
EOF

    (cd "{{WEB_DIR}}" && npm run dev)
  else
    cat <<'EOF'
==> No `web/` app detected.

Falling back to the browser-memory proof-of-concept server, which sets COOP/COEP
headers so `SharedArrayBuffer` is available.
EOF
    node poc/browser-memory/server.mjs
  fi

wasm-watch:
  #!/usr/bin/env bash
  set -euo pipefail

  wasm_dir="$(just _detect_wasm_crate_dir)"
  if [[ -z "${wasm_dir}" ]]; then
    echo "==> wasm-watch: no Rust wasm crate found (set WASM_CRATE_DIR=...); nothing to watch"
    exit 0
  fi

  if ! command -v watchexec >/dev/null; then
    cat >&2 <<'EOF'
error: watchexec is required for wasm-watch.

Install it with:
  cargo install watchexec-cli
EOF
    exit 1
  fi

  echo "==> Watching '${wasm_dir}' and rebuilding wasm on changes..."
  watchexec -w "${wasm_dir}" -- just wasm-threaded

build:
  #!/usr/bin/env bash
  set -euo pipefail

  echo "==> Building wasm (single + threaded)"
  just wasm

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    echo "==> Building web bundle (production)"
    (cd "{{WEB_DIR}}" && npm run build)
  else
    echo "==> Web: '{{WEB_DIR}}/package.json' not found; skipping web build"
  fi

test:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -f "Cargo.toml" ]]; then
    echo "==> Rust: cargo test"
    cargo test
  else
    echo "==> Rust: Cargo.toml not found; skipping cargo test"
  fi

  just _maybe_run_web_script test

fmt:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -f "Cargo.toml" ]]; then
    echo "==> Rust: cargo fmt"
    cargo fmt --all
  else
    echo "==> Rust: Cargo.toml not found; skipping cargo fmt"
  fi

  just _maybe_run_web_script fmt

lint:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -f "Cargo.toml" ]]; then
    echo "==> Rust: cargo clippy"
    cargo clippy --all-targets --all-features
  else
    echo "==> Rust: Cargo.toml not found; skipping cargo clippy"
  fi

  just _maybe_run_web_script lint

object-store-up:
  #!/usr/bin/env bash
  set -euo pipefail
  if ! command -v docker >/dev/null; then
    echo "error: docker is required to run the local object store" >&2
    exit 1
  fi
  (cd infra/local-object-store && docker compose up)

object-store-up-proxy:
  #!/usr/bin/env bash
  set -euo pipefail
  if ! command -v docker >/dev/null; then
    echo "error: docker is required to run the local object store" >&2
    exit 1
  fi
  (cd infra/local-object-store && docker compose --profile proxy up)

object-store-down:
  #!/usr/bin/env bash
  set -euo pipefail
  if ! command -v docker >/dev/null; then
    echo "error: docker is required to manage the local object store" >&2
    exit 1
  fi
  (cd infra/local-object-store && docker compose down)

object-store-reset:
  #!/usr/bin/env bash
  set -euo pipefail
  if ! command -v docker >/dev/null; then
    echo "error: docker is required to manage the local object store" >&2
    exit 1
  fi
  (cd infra/local-object-store && docker compose down -v)

object-store-verify *args:
  #!/usr/bin/env bash
  set -euo pipefail
  (cd infra/local-object-store && ./verify.sh {{args}})
