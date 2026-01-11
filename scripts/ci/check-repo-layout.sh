#!/usr/bin/env bash
set -euo pipefail

# Repo layout guardrails.
#
# Goal:
# - Keep the repo from accidentally growing multiple "canonical" apps.
# - Make it explicit which Vite config is production vs harness.
#
# This script is intentionally lightweight and is safe to run in CI.

die() {
  echo "error: $*" >&2
  exit 1
}

need_file() {
  local path="$1"
  [[ -f "$path" ]] || die "expected file '$path' to exist"
}

cd "$(git rev-parse --show-toplevel)"

need_file "docs/repo-layout.md"
need_file "docs/adr/0001-repo-layout.md"

# Windows driver CI packaging template guardrails.
#
# These files are intended to be copied by new driver authors, so keep them present and
# keep the starter manifest demonstrating all supported keys (without opting into any
# Microsoft WDK redistributables by default).
need_file "drivers/_template/ci-package.README.md"
need_file "drivers/_template/ci-package.json"
need_file "drivers/_template/ci-package.inf-wow64-example.json"
need_file "drivers/_template/ci-package.wdf-example.json"

if command -v python3 >/dev/null 2>&1; then
  python3 - <<'PY'
import json
import sys

path = "drivers/_template/ci-package.json"
with open(path, "r", encoding="utf-8") as f:
    manifest = json.load(f)

required = ["infFiles", "wow64Files", "additionalFiles"]
missing = [k for k in required if k not in manifest]
if missing:
    raise SystemExit(f"{path}: missing required key(s): {', '.join(missing)}")

if "wdfCoInstaller" in manifest:
    raise SystemExit(f"{path}: must not include wdfCoInstaller by default (WDK redistributables are opt-in)")

inf_files = manifest.get("infFiles")
if not isinstance(inf_files, list) or not inf_files:
    raise SystemExit(f"{path}: infFiles must be a non-empty array (template should include a .inf placeholder)")
if not isinstance(inf_files[0], str) or not inf_files[0].lower().endswith(".inf"):
    raise SystemExit(f"{path}: infFiles placeholder must be a string ending in .inf; got: {inf_files[0]!r}")

for key in ("wow64Files", "additionalFiles"):
    value = manifest.get(key)
    if not isinstance(value, list):
        raise SystemExit(f"{path}: {key} must be an array; got: {type(value).__name__}")

print("Driver template manifest check: OK")
PY
else
  echo "warning: python3 not found; skipping driver template manifest check" >&2
fi

# Canonical shared protocol vectors (used by cross-language conformance tests).
need_file "protocol-vectors/README.md"
need_file "protocol-vectors/udp-relay.json"
need_file "protocol-vectors/tcp-mux-v1.json"
need_file "protocol-vectors/l2-tunnel-v1.json"

# Unified, versioned conformance vectors (protocols + auth) consumed by multiple implementations.
need_file "crates/conformance/test-vectors/README.md"
need_file "crates/conformance/test-vectors/aero-vectors-v1.json"

# Guardrail: avoid reintroducing a second, competing "canonical" vectors directory.
if [[ -d "tests/protocol-vectors" ]]; then
  die "tests/protocol-vectors is deprecated; use protocol-vectors/ instead"
fi

# Guardrail: an obsolete prototype GPU device crate must not be reintroduced.
# (The canonical AeroGPU protocol is A3A0; see drivers/aerogpu/protocol/.)
retired_gpu_device_dir="crates/aero-gpu""-device"
if [[ -d "$retired_gpu_device_dir" ]]; then
  die "$retired_gpu_device_dir is retired and must not exist in the repo"
fi
if grep -q "$retired_gpu_device_dir" Cargo.toml; then
  die "Cargo workspace must not include the retired $retired_gpu_device_dir member"
fi

# Guest Tools layout: canonical driver directory name is `aerogpu` (no hyphen), matching
# the INF naming (`aerogpu.inf`) and source tree (`drivers/aerogpu/`).
#
# The packager/validators accept `aero-gpu` as a legacy alias for *input* layouts, but we
# should not check in the legacy directory name (it tends to reappear via copy/paste).
guest_tools_aerogpu_dirs=(
  "guest-tools/drivers/amd64/aerogpu"
  "guest-tools/drivers/x86/aerogpu"
)
for d in "${guest_tools_aerogpu_dirs[@]}"; do
  if [[ ! -d "$d" ]]; then
    die "expected Guest Tools driver directory '$d' to exist (canonical AeroGPU dir name is 'aerogpu')"
  fi
done
guest_tools_aerogpu_legacy_dirs=(
  "guest-tools/drivers/amd64/aero-gpu"
  "guest-tools/drivers/x86/aero-gpu"
)
for d in "${guest_tools_aerogpu_legacy_dirs[@]}"; do
  if [[ -d "$d" ]]; then
    die "legacy Guest Tools driver dir '$d' found; rename to use the canonical 'aerogpu' directory name"
  fi
done

# AeroGPU shared-surface contract: `share_token` is persisted via WDDM allocation
# private driver data (dxgkrnl preserves the blob across OpenResource).
#
# `aerogpu_alloc.h` is a stable include path; `aerogpu_wddm_alloc.h` is the
# canonical definition.
need_file "drivers/aerogpu/protocol/aerogpu_alloc.h"
need_file "drivers/aerogpu/protocol/aerogpu_wddm_alloc.h"

# Guardrail: the repo must not reintroduce the deprecated
# `drivers/aerogpu/protocol/aerogpu_alloc_privdata.h` header (the removed
# "KMD→UMD ShareToken" model). The canonical cross-process token is stored in
# `aerogpu_wddm_alloc_priv.share_token` (`aerogpu_wddm_alloc.h`).
deprecated_alloc_privdata_header="drivers/aerogpu/protocol/aerogpu_alloc_privdata.h"
if [[ -f "$deprecated_alloc_privdata_header" ]]; then
  die "$deprecated_alloc_privdata_header is deprecated; use drivers/aerogpu/protocol/aerogpu_wddm_alloc.h instead"
fi
# Ban any stale references anywhere in the repo (including language mirrors), but
# exclude this guardrail script and the dedicated share-token contract checker
# which necessarily reference the banned identifier.
if git grep -n "aerogpu_alloc_privdata" -- \
  . \
  ':(exclude)scripts/ci/check-repo-layout.sh' \
  ':(exclude)scripts/ci/check-aerogpu-share-token-contract.py' \
  >/dev/null; then
  die "stale references to aerogpu_alloc_privdata found (use drivers/aerogpu/protocol/aerogpu_wddm_alloc.h instead)"
fi

# Win7 test suite docs: keep the README's "Expected results" list in sync with the
# test manifest so newly-added tests stay documented.
if command -v python3 >/dev/null 2>&1; then
  python3 - <<'PY'
import re
from pathlib import Path

suite_dir = Path("drivers/aerogpu/tests/win7")
manifest_path = suite_dir / "tests_manifest.txt"
readme_path = suite_dir / "README.md"
cmake_path = suite_dir / "CMakeLists.txt"
runner_path = suite_dir / "test_runner" / "main.cpp"

manifest_tests = []
for raw in manifest_path.read_text(encoding="utf-8").splitlines():
    line = raw.strip()
    if not line:
        continue
    # Mirror the Windows-side manifest parsing logic (tokens=1 with comment guards).
    token = line.split(None, 1)[0]
    if not token:
        continue
    if token[0] in ("#", ";") or token.startswith("::") or token.lower() == "rem":
        continue
    manifest_tests.append(token)

for test in manifest_tests:
    test_dir = suite_dir / test
    if not test_dir.is_dir():
        raise SystemExit(f"{manifest_path}: listed test directory does not exist: {test_dir}")
    build_script = test_dir / "build_vs2010.cmd"
    if not build_script.is_file():
        raise SystemExit(f"{manifest_path}: missing build_vs2010.cmd for test {test!r}: {build_script}")

mentioned = set()
for raw in readme_path.read_text(encoding="utf-8").splitlines():
    # Only consider top-level bullets (`* ...`), not nested bullets.
    m = re.match(r"^\* `([a-z0-9_]+)`", raw)
    if m:
        mentioned.add(m.group(1))

missing = [t for t in manifest_tests if t not in mentioned]
if missing:
    raise SystemExit(
        f"{readme_path}: missing Expected results bullets for manifest test(s): {', '.join(missing)}"
    )

cmake_targets = set(
    re.findall(r"^\s*aerogpu_add_win7_test\(\s*([A-Za-z0-9_]+)", cmake_path.read_text(encoding="utf-8"), re.M)
)
missing_cmake = [t for t in manifest_tests if t not in cmake_targets]
if missing_cmake:
    raise SystemExit(
        f"{cmake_path}: missing aerogpu_add_win7_test entries for manifest test(s): {', '.join(missing_cmake)}"
    )

# Warn on extra CMake targets that look like tests but are not listed in the
# manifest. Allow known helper binaries.
cmake_allowed_extras = {
    "aerogpu_timeout_runner",
    "aerogpu_test_runner",
    # Built only to support d3d9ex_shared_surface_wow64 (not a suite entry).
    "d3d9ex_shared_surface_wow64_consumer_x64",
}
extra_cmake = sorted((cmake_targets - cmake_allowed_extras) - set(manifest_tests))
if extra_cmake:
    raise SystemExit(
        f"{cmake_path}: aerogpu_add_win7_test target(s) not present in tests_manifest.txt: {', '.join(extra_cmake)}"
    )

# Ensure the built-in fallback list stays in sync with the manifest. This is used
# when tests_manifest.txt is not bundled with a binary-only distribution.
runner_text = runner_path.read_text(encoding="utf-8", errors="replace")
m = re.search(r"const\s+char\*\s+const\s+kFallbackTests\[\]\s*=\s*\{(.*?)\};", runner_text, re.S)
if not m:
    raise SystemExit(f"{runner_path}: could not find kFallbackTests[] fallback list")
fallback_tests_list = re.findall(r'"([a-z0-9_]+)"', m.group(1))
fallback_tests_set = set(fallback_tests_list)
if len(fallback_tests_set) != len(fallback_tests_list):
    raise SystemExit(f"{runner_path}: kFallbackTests[] contains duplicate entries")
missing_fallback = [t for t in manifest_tests if t not in fallback_tests_set]
extra_fallback = [t for t in fallback_tests_list if t not in set(manifest_tests)]
if missing_fallback or extra_fallback:
    msg = []
    if missing_fallback:
        msg.append(f"missing: {', '.join(missing_fallback)}")
    if extra_fallback:
        msg.append(f"extra: {', '.join(extra_fallback)}")
    raise SystemExit(f"{runner_path}: kFallbackTests[] does not match tests_manifest.txt ({'; '.join(msg)})")

# Keep the execution order stable: the fallback list should be identical to the
# manifest list, not just set-equal.
if fallback_tests_list != manifest_tests:
    for i, (got, want) in enumerate(zip(fallback_tests_list, manifest_tests)):
        if got != want:
            raise SystemExit(
                f"{runner_path}: kFallbackTests[] order mismatch at index {i}: got {got!r} expected {want!r}"
            )
    raise SystemExit(
        f"{runner_path}: kFallbackTests[] length/order mismatch (fallback={len(fallback_tests_list)} manifest={len(manifest_tests)})"
    )

print("Win7 test suite manifest/doc/cmake check: OK")
PY
else
  echo "warning: python3 not found; skipping Win7 test suite manifest/doc/cmake check" >&2
fi

# npm workspaces: enforce a single repo-root lockfile to prevent dependency drift.
# (Per-package lockfiles are ignored via .gitignore, but this catches forced adds.)
mapfile -t npm_lockfiles < <(git ls-files | grep -E '(^|/)package-lock\.json$' || true)
unexpected_lockfiles=()
for lf in "${npm_lockfiles[@]}"; do
  if [[ "$lf" != "package-lock.json" ]]; then
    unexpected_lockfiles+=("$lf")
  fi
done
if (( ${#unexpected_lockfiles[@]} > 0 )); then
  die "unexpected package-lock.json checked in outside the repo root (npm workspaces use a single root lockfile): ${unexpected_lockfiles[*]}"
fi

# Canonical frontend (ADR 0001): repo-root Vite app (used by CI/Playwright).
need_file "index.html"
need_file "src/main.ts"
need_file "vite.harness.config.ts"

# Shared web runtime + WASM build tooling (and a legacy/experimental Vite entrypoint).
need_file "web/package.json"
need_file "web/README.md"
if ! grep -q "legacy/experimental" web/README.md; then
  die "web/README.md should clearly mark the web/ Vite entrypoint as legacy/experimental"
fi
need_file "web/index.html"
need_file "web/vite.config.ts"

# Non-canonical prototype markers (repo hygiene).
need_file "poc/README.md"
need_file "prototype/README.md"
need_file "server/LEGACY.md"

# Repo-root Vite app should be explicitly marked so it is not mistaken for a prototype.
if [[ -f "index.html" ]]; then
  if ! grep -q "canonical browser host" index.html; then
    die "repo-root index.html exists but is not marked as the canonical browser host (expected the phrase 'canonical browser host')"
  fi
fi

if ! grep -q "repo-root Vite app" vite.harness.config.ts; then
  die "vite.harness.config.ts should include the phrase 'repo-root Vite app' to make its role unambiguous"
fi

# Legacy Windows driver layout guardrails.
#
# The repo used to have a standalone GitHub Actions workflow for building a legacy Windows
# driver stack. It was removed in favor of the consolidated Win7 pipeline:
#   .github/workflows/drivers-win7.yml + ci/*.ps1 + drivers/*
legacy_windows_driver_workflow=".github/workflows/windows-""drivers.yml"
if [[ -f "$legacy_windows_driver_workflow" ]]; then
  die "legacy Windows driver workflow must not exist (use '.github/workflows/drivers-win7.yml')"
fi
legacy_guest_windows_dir="guest/""windows"
if [[ -d "$legacy_guest_windows_dir" ]]; then
  # The legacy driver directory is kept as a tombstone for old links. It must remain a stub
  # (no buildable driver projects). We allow a tiny set of redirect/stub files so older
  # links keep working. A comment-only stub INF is allowed so references to the old
  # `guest/` `windows/inf/aerogpu.inf` path fail loudly while still pointing at the supported
  # package location.
  allowed_guest_windows_files=(
    "guest/""windows/README.md"
    "guest/""windows/docs/driver_install.md"
    "guest/""windows/inf/aerogpu.inf"
  )

  # Use a simple prefix scan instead of relying on pathspec glob support (`**`).
  legacy_guest_windows_prefix="$legacy_guest_windows_dir/"
  guest_windows_files=()
  while IFS= read -r f; do
    if [[ "$f" == "$legacy_guest_windows_prefix"* ]]; then
      guest_windows_files+=("$f")
    fi
  done < <(git ls-files || true)
  for f in "${guest_windows_files[@]}"; do
    allowed=0
    for allow in "${allowed_guest_windows_files[@]}"; do
      if [[ "$f" == "$allow" ]]; then
        allowed=1
        break
      fi
    done
    if [[ "$allowed" -ne 1 ]]; then
      die "unexpected file under ${legacy_guest_windows_dir}/ (tombstone should only contain README stub + driver_install stub + INF stub): $f"
    fi
  done
fi

# Fail if someone reintroduces an ambiguous Vite config file name at the repo root
# (it would be auto-picked up by `vite` and confuse dev/CI tooling).
if [[ -f "vite.config.ts" || -f "vite.config.js" || -f "vite.config.mjs" || -f "vite.config.cjs" ]]; then
  die "unexpected Vite config at repo root (vite.config.*). Use vite.harness.config.ts for the canonical repo-root app (and web/vite.config.ts only for the legacy web/ app)."
fi

# Fail if any new Vite config is introduced outside the allowlist.
mapfile -t vite_configs < <(git ls-files | grep -E '(^|/)vite\.config\.(ts|js|mjs|cjs)$' || true)
allowed_vite_configs=(
  "web/vite.config.ts"
)
for cfg in "${vite_configs[@]}"; do
  allowed=0
  for allow in "${allowed_vite_configs[@]}"; do
    if [[ "$cfg" == "$allow" ]]; then
      allowed=1
      break
    fi
  done
  if [[ "$allowed" -ne 1 ]]; then
    die "unexpected Vite config file '$cfg' (if this is intentional, add an ADR + update scripts/ci/check-repo-layout.sh)"
  fi
done

echo "Repo layout check: OK"
