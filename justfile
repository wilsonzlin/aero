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

  args=(--allow-missing)
  if [[ -n "${WASM_CRATE_DIR:-}" ]]; then
    args+=(--wasm-crate-dir "${WASM_CRATE_DIR}")
  elif [[ -n "${AERO_WASM_CRATE_DIR:-}" ]]; then
    args+=(--wasm-crate-dir "${AERO_WASM_CRATE_DIR}")
  fi

  out="$(./scripts/ci/detect-wasm-crate.sh "${args[@]}")"
  dir=""
  while IFS="=" read -r key value; do
    case "$key" in
      dir)
        dir="$value"
        ;;
    esac
  done <<<"$out"

  echo "$dir"

default:
  @just --list

setup:
  #!/usr/bin/env bash
  set -euo pipefail

  echo "==> Rust: installing pinned toolchains + wasm32 target"
  if ! command -v rustup >/dev/null; then
    echo "error: rustup is required (https://rustup.rs)" >&2
    exit 1
  fi
  stable_toolchain="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]\+\)".*/\1/p' rust-toolchain.toml | head -n1)"
  if [[ -z "${stable_toolchain}" ]]; then
    echo "error: unable to determine [toolchain].channel from rust-toolchain.toml" >&2
    exit 1
  fi
  echo "==> Rust: ensuring toolchain '${stable_toolchain}' is installed"
  rustup toolchain install "${stable_toolchain}" --profile minimal
  rustup target add wasm32-unknown-unknown --toolchain "${stable_toolchain}"

  echo "==> Rust: installing formatter/linter components"
  rustup component add rustfmt clippy --toolchain "${stable_toolchain}"

  # Threaded/shared-memory WASM builds use `-Z build-std` and therefore need
  # nightly + rust-src. The web app's `npm run wasm:build` script depends on this.
  toolchains_file="scripts/toolchains.json"
  if [[ ! -f "${toolchains_file}" ]]; then
    echo "error: ${toolchains_file} not found (required to determine pinned nightly toolchain)" >&2
    exit 1
  fi
  wasm_nightly="$(sed -n 's/.*"nightlyWasm"[[:space:]]*:[[:space:]]*"\([^"]\+\)".*/\1/p' "${toolchains_file}" | head -n1)"
  if [[ -z "${wasm_nightly}" ]]; then
    echo "error: unable to determine rust.nightlyWasm from ${toolchains_file}" >&2
    exit 1
  fi

  echo "==> Rust: ensuring '${wasm_nightly}' + rust-src are available (for threaded WASM builds)"
  rustup toolchain install "${wasm_nightly}" --profile minimal
  rustup target add wasm32-unknown-unknown --toolchain "${wasm_nightly}"
  rustup component add rust-src --toolchain "${wasm_nightly}"

  wasm_dir="$(just _detect_wasm_crate_dir)"
  if [[ -n "${wasm_dir}" ]]; then
    echo "==> Tooling: checking wasm build tools"
    if ! command -v wasm-pack >/dev/null; then
      echo "error: wasm-pack is required to build the wasm package." >&2
      echo "" >&2
      echo "Install it with:" >&2
      echo "  cargo install wasm-pack" >&2
      exit 1
    fi

    # `wasm-bindgen-cli` is typically handled automatically by wasm-pack, but
    # some workflows may still want it installed globally.
    if ! command -v wasm-bindgen >/dev/null 2>&1; then
      echo "==> Tooling: wasm-bindgen-cli not found (usually OK; wasm-pack downloads its own binary)"
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
    if [[ ! -d "{{WEB_DIR}}/node_modules" ]]; then
      echo "error: '{{WEB_DIR}}/node_modules' not found; run 'just setup' first" >&2
      exit 1
    fi

    echo "==> Building wasm (single + threaded)"
    just wasm

    echo "==> Starting Vite dev server"
    echo ""
    echo "Vite will print the local URL (usually http://localhost:5173)."
    echo ""
    echo "Note: Aero relies on SharedArrayBuffer/WASM threads, which require cross-origin isolation"
    echo '(COOP/COEP headers => `crossOriginIsolated === true`). If you see'
    echo '`SharedArrayBuffer is not defined` or `crossOriginIsolated is false`, see the'
    echo "Troubleshooting section in README.md."

    wasm_dir="$(just _detect_wasm_crate_dir || true)"
    if [[ -n "${wasm_dir}" ]] && command -v watchexec >/dev/null 2>&1; then
      echo "==> watchexec detected; rebuilding threaded/shared-memory WASM on changes in '${wasm_dir}'"
      # Run the watcher in the background and kill it when the dev server exits.
      watchexec -w "${wasm_dir}" -- just wasm-threaded &
      watcher_pid=$!
      trap 'kill "${watcher_pid}" >/dev/null 2>&1 || true' EXIT
    else
      echo "==> Tip: For automatic wasm rebuilds while the dev server is running, use a second terminal:"
      echo "     just wasm-watch  # rebuilds the threaded/shared-memory WASM variant"
    fi

    (cd "{{WEB_DIR}}" && npm run dev)
  else
    echo '==> No `web/` app detected.'
    echo ""
    echo "Falling back to the browser-memory proof-of-concept server, which sets COOP/COEP"
    echo "headers so `SharedArrayBuffer` is available."
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
    echo "error: watchexec is required for wasm-watch." >&2
    echo "" >&2
    echo "Install it with:" >&2
    echo "  cargo install watchexec-cli" >&2
    exit 1
  fi

  echo "==> Watching '${wasm_dir}' and rebuilding threaded/shared-memory WASM on changes..."
  watchexec -w "${wasm_dir}" -- just wasm-threaded

build:
  #!/usr/bin/env bash
  set -euo pipefail

  echo "==> Building wasm (single + threaded)"
  just wasm

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if [[ ! -d "{{WEB_DIR}}/node_modules" ]]; then
      echo "error: '{{WEB_DIR}}/node_modules' not found; run 'just setup' first" >&2
      exit 1
    fi

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

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if [[ ! -d "{{WEB_DIR}}/node_modules" ]]; then
      echo "error: '{{WEB_DIR}}/node_modules' not found; run 'just setup' first" >&2
      exit 1
    fi
  fi

  # Prefer a dedicated unit-test script if available.
  if [[ -f "{{WEB_DIR}}/package.json" ]] && (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['test:unit']) ? 0 : 1)" >/dev/null 2>&1); then
    (cd "{{WEB_DIR}}" && npm run test:unit)
  else
    just _maybe_run_web_script test
  fi

test-all:
  ./scripts/test-all.sh

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

  # TypeScript typechecking is the closest thing we have to "lint" in web/.
  just _maybe_run_web_script typecheck
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
  (cd infra/local-object-store && docker compose --profile proxy down)

object-store-reset:
  #!/usr/bin/env bash
  set -euo pipefail
  if ! command -v docker >/dev/null; then
    echo "error: docker is required to manage the local object store" >&2
    exit 1
  fi
  (cd infra/local-object-store && docker compose --profile proxy down -v)

object-store-verify *args:
  #!/usr/bin/env bash
  set -euo pipefail
  (cd infra/local-object-store && ./verify.sh {{args}})
