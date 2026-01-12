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
#   # Run the legacy `web/` Vite app (the default uses the repo-root app).
#   AERO_NODE_DIR=web just dev
#   AERO_NODE_DIR=web AERO_WASM_CRATE_DIR=crates/aero-wasm just wasm-watch

set shell := ["bash", "-euo", "pipefail", "-c"]

# Auto-detect the Node workspace (repo root vs web/ vs other) so `just` stays in
# sync with CI and `./scripts/test-all.sh`.
#
# Canonical override: AERO_NODE_DIR. Deprecated: AERO_WEB_DIR, WEB_DIR.
#
# If `node` is unavailable (e.g. Rust-only workflows), fall back to a best-effort
# heuristic rather than failing justfile parsing.
WEB_DIR := env_var_or_default("AERO_NODE_DIR", env_var_or_default("AERO_WEB_DIR", env_var_or_default("WEB_DIR", `bash -c 'dir=""; if command -v node >/dev/null 2>&1 && [[ -f scripts/ci/detect-node-dir.mjs ]]; then dir="$(node scripts/ci/detect-node-dir.mjs 2>/dev/null | sed -n "s/^dir=//p" | head -n1 | tr -d "\\r" | xargs || true)"; fi; if [[ -n "$dir" ]]; then echo "$dir"; elif [[ -f package.json ]]; then echo .; elif [[ -f frontend/package.json ]]; then echo frontend; elif [[ -f web/package.json ]]; then echo web; else echo .; fi'`)))

[private]
_warn_deprecated_env:
  #!/usr/bin/env bash
  set -euo pipefail

  if [[ -n "${AERO_WEB_DIR:-}" && -z "${AERO_NODE_DIR:-}" ]]; then
    echo "warning: AERO_WEB_DIR is deprecated; use AERO_NODE_DIR instead" >&2
  fi
  if [[ -n "${WEB_DIR:-}" && -z "${AERO_NODE_DIR:-}" ]]; then
    echo "warning: WEB_DIR is deprecated; use AERO_NODE_DIR instead" >&2
  fi

  if [[ -n "${AERO_WASM_DIR:-}" && -z "${AERO_WASM_CRATE_DIR:-}" ]]; then
    echo "warning: AERO_WASM_DIR is deprecated; use AERO_WASM_CRATE_DIR instead" >&2
  fi
  if [[ -n "${WASM_CRATE_DIR:-}" && -z "${AERO_WASM_CRATE_DIR:-}" ]]; then
    echo "warning: WASM_CRATE_DIR is deprecated; use AERO_WASM_CRATE_DIR instead" >&2
  fi

[private]
_check_node_version:
  #!/usr/bin/env bash
  set -euo pipefail

  if ! command -v node >/dev/null 2>&1; then
    echo "error: missing required command: node" >&2
    exit 1
  fi

  node scripts/check-node-version.mjs

[private]
_detect_wasm_crate_dir:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env

  # Keep the justfile forgiving for partial setups: if Node is not installed yet,
  # treat the wasm crate as "not detected" (recipes that actually build WASM will
  # require Node anyway).
  if ! command -v node >/dev/null 2>&1; then
    echo ""
    exit 0
  fi

  args=(--allow-missing)
  if [[ -n "${AERO_WASM_CRATE_DIR:-}" ]]; then
    args+=(--wasm-crate-dir "${AERO_WASM_CRATE_DIR}")
  elif [[ -n "${AERO_WASM_DIR:-}" ]]; then
    args+=(--wasm-crate-dir "${AERO_WASM_DIR}")
  elif [[ -n "${WASM_CRATE_DIR:-}" ]]; then
    args+=(--wasm-crate-dir "${WASM_CRATE_DIR}")
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

  just _warn_deprecated_env

  echo "==> Rust: installing pinned toolchains + wasm32 target"
  if ! command -v rustup >/dev/null; then
    echo "error: rustup is required (https://rustup.rs)" >&2
    exit 1
  fi
  stable_toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]\+\)".*/\1/p' rust-toolchain.toml | head -n1)"
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
      echo "  cargo install --locked wasm-pack" >&2
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
    echo "==> Node: installing JS dependencies (npm ci)"
    just _check_node_version
    if ! command -v npm >/dev/null; then
      echo "error: npm is required to install JS deps" >&2
      exit 1
    fi

    # Workspaces: the lockfile lives at the repo root, even if users override
    # `AERO_NODE_DIR` to point at a workspace subdir (e.g. `web/`).
    #
    # Running `npm ci` inside a workspace subdir can create a nested
    # `web/node_modules` tree, which defeats the purpose of a single shared
    # workspace install.
    install_dir="{{WEB_DIR}}"
    if command -v node >/dev/null 2>&1 && [[ -f scripts/ci/detect-node-dir.mjs ]]; then
      lockfile="$(node scripts/ci/detect-node-dir.mjs --require-lockfile | sed -n 's/^lockfile=//p' | head -n1 | tr -d '\r' | xargs)"
      if [[ -n "$lockfile" ]]; then
        install_dir="$(dirname "$lockfile")"
      fi
    elif [[ -f package-lock.json ]]; then
      install_dir="."
    fi
    if [[ -z "$install_dir" ]]; then
      install_dir="."
    fi

    # Keep `just setup` fast by skipping the Playwright browser download. Install
    # browsers explicitly when you need to run E2E tests:
    #   npx playwright install --with-deps chromium
    (cd "$install_dir" && PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci)
  else
    echo "==> Node: '{{WEB_DIR}}/package.json' not found; skipping npm ci"
  fi

  echo "==> Setup complete"

wasm:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['wasm:build']) ? 0 : 1)" >/dev/null 2>&1); then
      if ! command -v wasm-pack >/dev/null; then
        echo "error: wasm-pack not found; run 'just setup' (or 'cargo install --locked wasm-pack')" >&2
        exit 1
      fi
      echo "==> Building WASM (single + threaded) via '{{WEB_DIR}}' npm scripts"
      just _check_node_version
      npm --prefix "{{WEB_DIR}}" run wasm:build
      exit 0
    fi
  fi

  # Fallback: build a single wasm-pack package directly from a detected crate.
  wasm_dir="$(just _detect_wasm_crate_dir)"
  if [[ -z "${wasm_dir}" ]]; then
    echo "==> wasm: no web wasm build scripts found and no Rust wasm crate detected (set AERO_WASM_CRATE_DIR=...); nothing to build"
    exit 0
  fi
  if ! command -v wasm-pack >/dev/null; then
    echo "error: wasm-pack not found; run 'just setup' (or 'cargo install --locked wasm-pack')" >&2
    exit 1
  fi
  out_dir="${WASM_PKG_DIR:-${wasm_dir}/pkg}"
  mkdir -p "${out_dir}"
  echo "==> Building wasm package (fallback): ${wasm_dir} -> ${out_dir}"
  wasm-pack build "${wasm_dir}" --dev --target web --out-dir "${out_dir}"

wasm-single:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['wasm:build:single']) ? 0 : 1)" >/dev/null 2>&1); then
      if ! command -v wasm-pack >/dev/null; then
        echo "error: wasm-pack not found; run 'just setup' (or 'cargo install --locked wasm-pack')" >&2
        exit 1
      fi
      echo "==> Building WASM (single) via '{{WEB_DIR}}' npm scripts"
      just _check_node_version
      npm --prefix "{{WEB_DIR}}" run wasm:build:single
      exit 0
    fi
  fi

  echo "==> wasm-single: web script not found; falling back to 'just wasm'"
  just wasm

wasm-threaded:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['wasm:build:threaded']) ? 0 : 1)" >/dev/null 2>&1); then
      if ! command -v wasm-pack >/dev/null; then
        echo "error: wasm-pack not found; run 'just setup' (or 'cargo install --locked wasm-pack')" >&2
        exit 1
      fi
      echo "==> Building WASM (threaded/shared-memory) via '{{WEB_DIR}}' npm scripts"
      just _check_node_version
      npm --prefix "{{WEB_DIR}}" run wasm:build:threaded
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
    npm --prefix "{{WEB_DIR}}" run "{{script}}"
  else
    echo "==> Web: no npm script named '{{script}}' (skipping)"
  fi

dev:
  #!/usr/bin/env bash
  set -euo pipefail

  just _check_node_version

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if [[ ! -d "node_modules" && ! -d "{{WEB_DIR}}/node_modules" ]]; then
      echo "error: 'node_modules' not found; run 'just setup' first" >&2
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

    npm --prefix "{{WEB_DIR}}" run dev
  else
    echo '==> No Node workspace detected.'
    echo ""
    echo "Falling back to the browser-memory proof-of-concept server, which sets COOP/COEP"
    echo "headers so `SharedArrayBuffer` is available."
    node poc/browser-memory/server.mjs
  fi

wasm-watch:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env

  wasm_dir="$(just _detect_wasm_crate_dir)"
  if [[ -z "${wasm_dir}" ]]; then
    echo "==> wasm-watch: no Rust wasm crate found (set AERO_WASM_CRATE_DIR=...); nothing to watch"
    exit 0
  fi

  if ! command -v watchexec >/dev/null; then
    echo "error: watchexec is required for wasm-watch." >&2
    echo "" >&2
    echo "Install it with:" >&2
    echo "  cargo install --locked watchexec-cli" >&2
    exit 1
  fi

  echo "==> Watching '${wasm_dir}' and rebuilding threaded/shared-memory WASM on changes..."
  watchexec -w "${wasm_dir}" -- just wasm-threaded

build:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env

  echo "==> Building wasm (single + threaded)"
  just wasm

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if [[ ! -d "node_modules" && ! -d "{{WEB_DIR}}/node_modules" ]]; then
      echo "error: 'node_modules' not found; run 'just setup' first" >&2
      exit 1
    fi

    just _check_node_version
    echo "==> Building web bundle (production)"
    npm --prefix "{{WEB_DIR}}" run build
  else
    echo "==> Web: '{{WEB_DIR}}/package.json' not found; skipping web build"
  fi

test:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env

  if [[ -f "Cargo.toml" ]]; then
    echo "==> Rust: cargo test"
    cargo test --locked
  else
    echo "==> Rust: Cargo.toml not found; skipping cargo test"
  fi

  if [[ -f "{{WEB_DIR}}/package.json" ]]; then
    if [[ ! -d "node_modules" && ! -d "{{WEB_DIR}}/node_modules" ]]; then
      echo "error: 'node_modules' not found; run 'just setup' first" >&2
      exit 1
    fi
  fi

  # Prefer a dedicated unit-test script if available.
  if [[ -f "{{WEB_DIR}}/package.json" ]] && (cd "{{WEB_DIR}}" && node -e "const p=require('./package.json'); process.exit((p.scripts && p.scripts['test:unit']) ? 0 : 1)" >/dev/null 2>&1); then
    npm --prefix "{{WEB_DIR}}" run test:unit
  else
    just _maybe_run_web_script test
  fi

test-all:
  cargo xtask test-all

gen-scancodes:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env
  just _check_node_version

  node tools/gen_scancodes/gen_scancodes.mjs

check-scancodes:
  #!/usr/bin/env bash
  set -euo pipefail

  just _warn_deprecated_env
  just _check_node_version

  node tools/gen_scancodes/check_generated.mjs

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
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
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

# Reproduce `.github/workflows/iac.yml` locally (Terraform/tflint + Helm/kubeconform + deploy hygiene).
check-iac:
  #!/usr/bin/env bash
  set -euo pipefail
  ./scripts/ci/check-iac.sh
