#!/usr/bin/env bash
set -euo pipefail

# Run any command under OS-enforced resource limits.
#
# DEFENSIVE: Assumes the command may consume infinite memory if unconstrained.
#
# Uses RLIMIT_AS (virtual address space) via prlimit or ulimit. This is simpler
# and more portable than cgroups/systemd-run, and works in most environments
# including containers and CI.
#
# Examples:
#   bash scripts/run_limited.sh --as 12G -- cargo build --release --locked
#   LIMIT_AS=12G bash scripts/run_limited.sh -- cargo build --release --locked
#   bash scripts/run_limited.sh --as 64G -- ./target/release/mybin

usage() {
  cat <<'EOF'
usage: scripts/run_limited.sh [--as <size>] [--stack <size>] [--cpu <secs>] -- <command...>

Limits:
  --as <size>     Address-space (virtual memory) limit. Example: 12G, 8192M.
  --stack <size>  Stack size limit.
  --cpu <secs>    CPU time limit (seconds).

Environment defaults (optional):
  LIMIT_AS, LIMIT_STACK, LIMIT_CPU

Notes:
  - `--as` (RLIMIT_AS) is the most reliable "hard memory ceiling" on Linux.
  - Size strings: 12G, 8192M, 4096K, or raw bytes.
  - If prlimit is missing, falls back to ulimit.
  - On Windows (Git Bash/MSYS), limits are not enforced (runs command directly).
EOF
}

to_kib() {
  local raw="${1:-}"
  raw="${raw//[[:space:]]/}"
  # Lowercase (portable across Bash 3.x).
  raw="$(printf '%s' "${raw}" | tr '[:upper:]' '[:lower:]')"

  # Accept common suffixes: k, m, g, t (optionally with b/ib).
  raw="${raw%ib}"
  raw="${raw%b}"

  if [[ "${raw}" =~ ^[0-9]+$ ]]; then
    # Fallback: treat as MiB (human-friendly for ulimit -v/-s which expect KiB).
    echo $((raw * 1024))
    return 0
  fi

  if [[ "${raw}" =~ ^([0-9]+)([kmgt])$ ]]; then
    local n="${BASH_REMATCH[1]}"
    local unit="${BASH_REMATCH[2]}"
    case "${unit}" in
      k) echo $((n)) ;;
      m) echo $((n * 1024)) ;;
      g) echo $((n * 1024 * 1024)) ;;
      t) echo $((n * 1024 * 1024 * 1024)) ;;
      *) return 1 ;;
    esac
    return 0
  fi

  return 1
}

to_bytes() {
  local kib
  kib="$(to_kib "${1:-}")" || return 1
  echo $((kib * 1024))
}

# Defaults
AS="${LIMIT_AS:-12G}"
STACK="${LIMIT_STACK:-}"
CPU="${LIMIT_CPU:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --as)
      AS="${2:-}"; shift 2 ;;
    --stack)
      STACK="${2:-}"; shift 2 ;;
    --cpu)
      CPU="${2:-}"; shift 2 ;;
    --no-as)
      AS=""; shift ;;
    --no-stack)
      STACK=""; shift ;;
    --no-cpu)
      CPU=""; shift ;;
    --)
      shift
      break
      ;;
    *)
      # No more wrapper flags; treat rest as the command.
      break
      ;;
  esac
done

if [[ $# -lt 1 ]]; then
  usage
  exit 2
fi

cmd=("$@")

# Windows (Git Bash / MSYS / Cygwin): resource limits don't work reliably.
# Run command directly without limits.
uname_s="$(uname -s 2>/dev/null || echo "")"
case "${uname_s}" in
  MINGW*|MSYS*|CYGWIN*)
    exec "${cmd[@]}"
    ;;
esac

# Check if any limits are requested
any_limit=false
if [[ -n "${AS}" && "${AS}" != "0" && "${AS}" != "unlimited" ]]; then any_limit=true; fi
if [[ -n "${STACK}" && "${STACK}" != "0" && "${STACK}" != "unlimited" ]]; then any_limit=true; fi
if [[ -n "${CPU}" && "${CPU}" != "0" && "${CPU}" != "unlimited" ]]; then any_limit=true; fi

if [[ "${any_limit}" == "false" ]]; then
  exec "${cmd[@]}"
fi

# Rustup's shim binaries (`cargo`, `rustc`, ...) reserve large virtual address space.
# Resolve `cargo` to the actual toolchain executable before applying limits.
if [[ -n "${AS}" && "${AS}" != "0" && "${AS}" != "unlimited" ]] \
  && [[ "${cmd[0]}" == "cargo" ]] \
  && command -v rustup >/dev/null 2>&1
then
  cargo_shim="$(command -v cargo || true)"
  if [[ -n "${cargo_shim}" ]]; then
    cargo_target="${cargo_shim}"
    if [[ -L "${cargo_shim}" ]]; then
      cargo_target="$(readlink "${cargo_shim}" 2>/dev/null || echo "${cargo_shim}")"
    fi

    if [[ "${cargo_target}" == "rustup" || "${cargo_target}" == */rustup || "${cargo_shim}" == */.cargo/bin/cargo ]]; then
      toolchain=""
      if [[ ${#cmd[@]} -gt 1 && "${cmd[1]}" == +* ]]; then
        toolchain="${cmd[1]#+}"
        cmd=("${cmd[0]}" "${cmd[@]:2}")
      fi

      if [[ -n "${toolchain}" ]]; then
        resolved="$(rustup which --toolchain "${toolchain}" cargo 2>/dev/null || true)"
      else
        resolved="$(rustup which cargo 2>/dev/null || true)"
      fi

      if [[ -n "${resolved}" ]]; then
        cmd[0]="${resolved}"
        toolchain_bin="$(dirname "${resolved}")"
        export PATH="${toolchain_bin}:${PATH}"
      fi
    fi
  fi
fi

# Try prlimit first (preferred: sets limits on current process, inherited by exec)
prlimit_ok=0
if command -v prlimit >/dev/null 2>&1; then
  # Test if prlimit works (some CI environments have broken prlimit)
  if prlimit --as=67108864 --cpu=1 -- true >/dev/null 2>&1; then
    prlimit_ok=1
  fi
fi

if [[ "${prlimit_ok}" -eq 1 ]]; then
  pl=(prlimit --pid $$)
  
  if [[ -n "${AS}" && "${AS}" != "0" ]]; then
    if [[ "${AS}" == "unlimited" ]]; then
      pl+=(--as=unlimited)
    else
      as_bytes="$(to_bytes "${AS}")" || {
        echo "[run_limited] invalid --as size: ${AS}" >&2
        exit 2
      }
      pl+=(--as="${as_bytes}")
    fi
  fi
  
  if [[ -n "${STACK}" && "${STACK}" != "0" ]]; then
    if [[ "${STACK}" == "unlimited" ]]; then
      pl+=(--stack=unlimited)
    else
      stack_bytes="$(to_bytes "${STACK}")" || {
        echo "[run_limited] invalid --stack size: ${STACK}" >&2
        exit 2
      }
      pl+=(--stack="${stack_bytes}")
    fi
  fi
  
  if [[ -n "${CPU}" && "${CPU}" != "0" ]]; then
    if [[ "${CPU}" == "unlimited" ]]; then
      pl+=(--cpu=unlimited)
    else
      if ! [[ "${CPU}" =~ ^[0-9]+$ ]]; then
        echo "[run_limited] invalid --cpu seconds: ${CPU}" >&2
        exit 2
      fi
      pl+=(--cpu="${CPU}")
    fi
  fi

  if "${pl[@]}" >/dev/null 2>&1; then
    exec "${cmd[@]}"
  fi
fi

# Fallback: ulimit (works on most systems including macOS)
if [[ -n "${AS}" && "${AS}" != "0" ]]; then
  if [[ "${AS}" == "unlimited" ]]; then
    ulimit -v unlimited 2>/dev/null || true
  else
    as_kib="$(to_kib "${AS}")" || {
      echo "[run_limited] invalid --as size: ${AS}" >&2
      exit 2
    }
    ulimit -v "${as_kib}" 2>/dev/null || true
  fi
fi

if [[ -n "${STACK}" && "${STACK}" != "0" ]]; then
  if [[ "${STACK}" == "unlimited" ]]; then
    ulimit -s unlimited 2>/dev/null || true
  else
    stack_kib="$(to_kib "${STACK}")" || {
      echo "[run_limited] invalid --stack size: ${STACK}" >&2
      exit 2
    }
    ulimit -s "${stack_kib}" 2>/dev/null || true
  fi
fi

if [[ -n "${CPU}" && "${CPU}" != "0" ]]; then
  if [[ "${CPU}" == "unlimited" ]]; then
    ulimit -t unlimited 2>/dev/null || true
  else
    if ! [[ "${CPU}" =~ ^[0-9]+$ ]]; then
      echo "[run_limited] invalid --cpu seconds: ${CPU}" >&2
      exit 2
    fi
    ulimit -t "${CPU}" 2>/dev/null || true
  fi
fi

exec "${cmd[@]}"
