#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
tests_dir="${script_dir}"
src_dir="$(cd -- "${script_dir}/../src" >/dev/null 2>&1 && pwd)"

tmp_root="${TMPDIR:-/tmp}"
build_dir="$(mktemp -d "${tmp_root%/}/virtio-input-tests.XXXXXX")"
trap 'rm -rf "${build_dir}"' EXIT

cflags=(-std=c11 -Wall -Wextra -Werror)

append_source() {
  local file="$1"
  local existing
  for existing in "${sources[@]}"; do
    if [[ "${existing}" == "${file}" ]]; then
      return 0
    fi
  done
  sources+=("${file}")
}

compilers=()
if [[ -n "${CC:-}" ]]; then
  compilers+=("${CC}")
else
  for c in gcc clang; do
    if command -v "${c}" >/dev/null 2>&1; then
      compilers+=("${c}")
    fi
  done
fi

if [[ ${#compilers[@]} -eq 0 ]]; then
  echo "error: no compiler found (gcc/clang). Set CC to override." >&2
  exit 1
fi

shopt -s nullglob
test_sources=("${tests_dir}"/*_test.c)
shopt -u nullglob

if [[ ${#test_sources[@]} -eq 0 ]]; then
  echo "error: no *_test.c files found in ${tests_dir}" >&2
  exit 1
fi

total_runs=0
failed_runs=0
fail_list=()

echo "== virtio-input host-side unit tests =="
echo "build dir: ${build_dir}"
echo

for cc in "${compilers[@]}"; do
  cc_label="$(basename "${cc}")"
  echo "-- compiler: ${cc_label} --"

  for test_src in "${test_sources[@]}"; do
    test_file="$(basename "${test_src}")"
    test_base="${test_file%.c}"
    module="${test_base%_test}"

    bin="${build_dir}/${test_base}.${cc_label}"
    sources=()
    append_source "${test_src}"

    # Convention: if ../src/<name>.c exists (where <name> is the test filename
    # without the "_test" suffix), link it in automatically.
    if [[ -f "${src_dir}/${module}.c" ]]; then
      append_source "${src_dir}/${module}.c"
    fi

    # Optional: allow tests to declare extra translation units to link.
    # Syntax (single line anywhere in the file):
    #   // TEST_DEPS: foo.c bar.c
    # or:
    #   // TEST_DEPS: foo bar   (".c" is implied)
    deps_line="$(sed -n 's,^//[[:space:]]*TEST_DEPS:[[:space:]]*,,p' "${test_src}" | head -n 1 || true)"
    if [[ -n "${deps_line}" ]]; then
      read -r -a deps <<<"${deps_line}"
      dep_missing=""
      for dep in "${deps[@]}"; do
        dep_file="${dep}"
        if [[ "${dep_file}" != *.c ]]; then
          dep_file="${dep_file}.c"
        fi
        dep_path="${src_dir}/${dep_file}"
        if [[ -f "${dep_path}" ]]; then
          append_source "${dep_path}"
        else
          dep_missing="${dep_file}"
          break
        fi
      done
      if [[ -n "${dep_missing}" ]]; then
        echo "[FAIL] build: ${test_file} (${cc_label})"
        echo "       missing dependency: ${src_dir}/${dep_missing}" >&2
        failed_runs=$((failed_runs + 1))
        total_runs=$((total_runs + 1))
        fail_list+=("BUILD ${test_file} (${cc_label})")
        continue
      fi
    fi

    echo "[BUILD] ${test_file}"
    if ! "${cc}" "${cflags[@]}" -o "${bin}" "${sources[@]}"; then
      echo "[FAIL] build: ${test_file} (${cc_label})"
      failed_runs=$((failed_runs + 1))
      total_runs=$((total_runs + 1))
      fail_list+=("BUILD ${test_file} (${cc_label})")
      continue
    fi

    echo "[RUN ] ${test_base}"
    if ! "${bin}"; then
      echo "[FAIL] run: ${test_base} (${cc_label})"
      failed_runs=$((failed_runs + 1))
      total_runs=$((total_runs + 1))
      fail_list+=("RUN ${test_base} (${cc_label})")
      continue
    fi

    echo "[PASS] ${test_base} (${cc_label})"
    total_runs=$((total_runs + 1))
  done

  echo
done

if [[ ${failed_runs} -eq 0 ]]; then
  echo "PASS: ${total_runs} test run(s)"
  exit 0
fi

echo "FAIL: ${failed_runs}/${total_runs} test run(s) failed"
for entry in "${fail_list[@]}"; do
  echo " - ${entry}"
done
exit 1
