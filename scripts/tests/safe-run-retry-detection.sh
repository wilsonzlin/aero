#!/usr/bin/env bash
# Regression test for scripts/safe-run.sh rustc EAGAIN/WouldBlock retry detection.
#
# Run:
#   bash ./scripts/tests/safe-run-retry-detection.sh
#
# This test synthesizes representative stderr logs and asserts that
# should_retry_rustc_thread_error() flags them as retryable.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# Source safe-run.sh without executing its main routine.
# shellcheck source=../safe-run.sh
source "${REPO_ROOT}/scripts/safe-run.sh"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/aero-safe-run-test.XXXXXX")"
cleanup() {
    rm -rf "${tmpdir}"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_retry() {
    local name="$1"
    local file="${tmpdir}/${name}.stderr"
    cat >"${file}"

    if should_retry_rustc_thread_error "${file}"; then
        echo "[ok] ${name}"
    else
        echo "---- stderr log (${name}) ----" >&2
        cat "${file}" >&2
        echo "------------------------------" >&2
        fail "expected retry for ${name}"
    fi
}

assert_no_retry() {
    local name="$1"
    local file="${tmpdir}/${name}.stderr"
    cat >"${file}"

    if should_retry_rustc_thread_error "${file}"; then
        echo "---- stderr log (${name}) ----" >&2
        cat "${file}" >&2
        echo "------------------------------" >&2
        fail "expected no retry for ${name}"
    else
        echo "[ok] ${name}"
    fi
}

assert_retry "panic-unwrap-eagain" <<'EOF'
thread 'rustc' panicked at 'called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }', library/core/src/result.rs:1:1
EOF

assert_retry "panic-unwrap-eagain-backticks" <<'EOF'
thread 'rustc' panicked at 'called `Result::unwrap()` on an `Err` value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }', library/core/src/result.rs:1:1
EOF

assert_retry "unwrap-eagain-only" <<'EOF'
called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
EOF

assert_no_retry "unwrap-non-eagain-only" <<'EOF'
called Result::unwrap() on an Err value: Os { code: 2, kind: NotFound, message: "No such file or directory" }
EOF

assert_retry "failed-to-spawn-eagain" <<'EOF'
error: internal compiler error: unexpected panic
failed to spawn helper thread: WouldBlock (os error 11)
EOF

assert_retry "ctrlc-handler-eagain" <<'EOF'
error: internal compiler error: unexpected panic
Unable to install ctrlc handler: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
EOF

assert_retry "could-not-execute-process-eagain" <<'EOF'
error: could not compile `foo` (lib)

Caused by:
  could not execute process `rustc --crate-name foo --print=file-names` (never executed)

Caused by:
  Resource temporarily unavailable (os error 11)
EOF

assert_retry "failed-to-fork-eagain" <<'EOF'
error: could not compile `foo` (build script)
Caused by:
  failed to fork: Resource temporarily unavailable (os error 11)
EOF

assert_retry "std-system-error-eagain" <<'EOF'
std::system_error: Resource temporarily unavailable
EOF

assert_no_retry "random-eagain-no-context" <<'EOF'
Resource temporarily unavailable
EOF

assert_retry "threadpoolbuilderror-eagain" <<'EOF'
ThreadPoolBuildError { kind: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" } }
EOF

assert_no_retry "panic-non-eagain" <<'EOF'
thread 'rustc' panicked at 'called Result::unwrap() on an Err value: Os { code: 2, kind: NotFound, message: "No such file or directory" }', library/core/src/result.rs:1:1
EOF

echo "All safe-run retry detection checks passed."
