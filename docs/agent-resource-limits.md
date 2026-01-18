# Agent Resource Limits & Concurrency Guide

**Audience:** Coding agents developing Aero (not end-users or CI runners).

**Goal:** Maximum speed without OOM-killing the shared host.

---

## 🛡️ Core Principle: Assume Hostile Processes

**Every process you spawn is potentially hostile, pathological, or malfunctioning.**

This isn't paranoia—it's operational reality. `cargo`, `rustc`, `node`, `npm`, `chromium`, and even simple shell commands can:

| Failure Mode | Example | Your Defense |
|--------------|---------|--------------|
| **Hang forever** | rustc stuck on codegen, npm waiting for network | Always use timeouts |
| **Consume infinite memory** | LTO linking, webpack bundling | Always use memory limits |
| **Spin CPU forever** | Infinite loop in proc macro, bad regex | Timeouts + kill |
| **Ignore SIGTERM** | Misbehaving Chrome process | SIGTERM → wait → SIGKILL |
| **Leave zombies** | Crashed parent, orphaned children | Kill process groups, not just PIDs |
| **Corrupt state** | Partial writes, lock files | Validate outputs, clean state |
| **Lie about success** | Exit 0 but wrong output | Check outputs, not just exit codes |

**Your code is not special.** It will also misbehave. Defend against yourself.

---

## The One Rule That Matters

**Memory is the constraint.** CPU and disk I/O are handled gracefully by the Linux scheduler under contention. But if 200 agents each try to use 16 GB during a Rust build, the machine will OOM and become unresponsive.

**Target:** ~6-8 GB typical, 12 GB hard ceiling per agent.

Everything else in this doc is secondary to this.

---

## Quick Setup

Run this once per checkout (sanity checks and prints activation instructions):

```bash
# From repo root
bash ./scripts/agent-env-setup.sh
```

> Troubleshooting: in some agent environments, the working tree can lose executable bits and/or be missing tracked fixtures.
>
> - If you get `Permission denied` running `./scripts/*.sh`, run via bash: `bash ./scripts/agent-env-setup.sh` / `bash ./scripts/safe-run.sh …`.
> - If `git status` shows many mode-only changes or deleted/empty tracked files, restore the checkout:
>   - `git checkout -- .` (bigger hammer), or at least:
>   - `git checkout -- scripts tools/packaging/aero_packager/testdata tools/disk-streaming-browser-e2e/fixtures`
>   - Non-git fallback: `find scripts -name '*.sh' -exec chmod +x {} +`

Then activate the recommended environment in your current shell:

```bash
source ./scripts/agent-env.sh
```

---

## Memory Limit Enforcement

We use RLIMIT_AS (virtual address space limit) via `prlimit` or `ulimit`. This is simpler and more portable than cgroups/systemd-run.

### RLIMIT_AS caveat (Node/V8/WebAssembly)

RLIMIT_AS limits **virtual address space**, not resident memory (RSS). Some runtimes reserve large virtual ranges up-front (especially Node/V8 and WebAssembly memories). Under the default `12G` cap, you may see spurious failures like:

- `WebAssembly.Instance(): Out of memory: Cannot allocate Wasm memory for new instance`
- `WebAssembly.Memory(): could not allocate memory`

Note: `scripts/safe-run.sh` will **auto-bump** its default address-space cap for **Node/WASM-heavy test entrypoints** (and Playwright) when `AERO_MEM_LIMIT` is unset. This is why you may see `Memory: 256G` in safe-run logs even though the general default is `12G`.

If you hit this while running JS/TS tooling (for example `npm -w web run test:unit`), re-run with a larger address-space limit for that command (or disable it) while keeping the timeout:

```bash
# Try raising the cap first
AERO_TIMEOUT=600 AERO_MEM_LIMIT=32G bash ./scripts/safe-run.sh npm -w web run test:unit

# If it still fails, disable RLIMIT_AS for that command (still keeps the timeout)
AERO_TIMEOUT=600 AERO_MEM_LIMIT=unlimited bash ./scripts/safe-run.sh npm -w web run test:unit

# Alternative (no address-space limit, but keep a timeout):
bash ./scripts/with-timeout.sh 600 bash ./scripts/run_limited.sh --no-as -- npm -w web run test:unit
```

### RLIMIT_AS caveat (rustc/LLVM)

Rust builds can also consume large **virtual address space** (via mmap/allocators), even when the
actual resident memory usage is reasonable. In some sandboxes this can surface as rustc panics like:

- `failed to spawn helper thread: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }`
- `thread 'rustc' panicked at 'called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }'`

If you hit this under `safe-run.sh`, re-run the command with a larger address-space limit:

```bash
# Common fix for large Cargo builds/tests
AERO_TIMEOUT=900 AERO_MEM_LIMIT=32G bash ./scripts/safe-run.sh cargo test --locked

# If your sandbox allows it, you can also disable RLIMIT_AS for that command:
AERO_TIMEOUT=900 AERO_MEM_LIMIT=unlimited bash ./scripts/safe-run.sh cargo test --locked
```

### Linker caveat (lld threads + wasm `RUSTFLAGS` gotcha)

On Linux, the pinned Rust toolchain links via **LLVM lld** (`-fuse-ld=lld`). lld defaults to using
all available hardware threads and can hit per-user thread limits under shared-host contention.

To keep builds reliable, `scripts/agent-env.sh` and `scripts/safe-run.sh` cap lld’s parallelism via
Cargo’s **per-target rustflags** environment variables:

```
CARGO_TARGET_<TRIPLE>_RUSTFLAGS="... -C link-arg=-Wl,--threads=<n>"
```

For **wasm32** targets, rustc invokes `rust-lld -flavor wasm` directly, so the native `-Wl,` prefix
is invalid. Use the wasm-compatible form:

```
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="... -C link-arg=--threads=<n>"
```

Avoid setting linker thread caps via **global `RUSTFLAGS`** (or `CARGO_ENCODED_RUSTFLAGS`), e.g.:

```bash
# ⚠️ Avoid: breaks wasm builds because rust-lld doesn't understand -Wl,
export RUSTFLAGS="-C link-arg=-Wl,--threads=1"
```

If you need to change the cap, prefer adjusting build parallelism instead:

```bash
export AERO_CARGO_BUILD_JOBS=2
source ./scripts/agent-env.sh
```

### Using `run_limited.sh` (Recommended)

```bash
# Limit to 12GB virtual address space
bash ./scripts/run_limited.sh --as 12G -- cargo build --release --locked

# Or use safe-run.sh which combines timeout + memory limit
bash ./scripts/safe-run.sh cargo build --release --locked
```

### How it works

1. **`prlimit`** (preferred): Sets RLIMIT_AS on the current process, inherited by children
2. **`ulimit -v`** (fallback): Same effect via shell builtin

This approach:
- Works in containers (no cgroups/systemd needed)
- Works on most Linux systems without root
- Works on macOS (via ulimit)
- Handles Rustup shims correctly (resolves to real cargo binary first)

### Soft limits (reduce peak usage)

These don't enforce hard limits but reduce memory spikes:

```bash
export CARGO_BUILD_JOBS=1       # Limit parallel rustc (agent default; raise if your sandbox allows)
export RUSTC_WORKER_THREADS=1   # Limit rustc's internal worker pool (avoid "WouldBlock" rustc ICEs)
export RAYON_NUM_THREADS=1      # Keep rustc/Rayon pools aligned with Cargo parallelism
export RUST_TEST_THREADS=1      # Limit Rust's built-in test harness parallelism (libtest)
export NEXTEST_TEST_THREADS=1   # Limit cargo-nextest test concurrency (if using cargo nextest)
export AERO_TOKIO_WORKER_THREADS=1  # Limit Tokio runtime worker threads for supported Aero binaries
export AERO_RUST_CODEGEN_UNITS=1  # Optional: reduce per-crate parallelism (slower, but can help under tight thread/process limits); alias: AERO_CODEGEN_UNITS
```

`bash ./scripts/safe-run.sh` also includes a small backoff + retry loop for Rust build/test commands
(Cargo and common wrappers like `npm`/`wasm-pack`) when it detects transient `rustc` thread-spawn
panics (e.g. `failed to spawn helper thread (WouldBlock)` or `called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }`), which can happen when many agents share
the same host. Override with
`AERO_SAFE_RUN_RUSTC_RETRIES=1` to disable retries.

---

## Recommended Build Settings

These balance speed with reasonable memory usage. They're defaults, not hard constraints—override if you know what you're doing.

### Common agent-sandbox failures (and what to do)

On shared hosts running many agents concurrently, Linux per-user process/thread limits can be hit even when
`CARGO_BUILD_JOBS=1`. When that happens you may see transient errors like:

- `Resource temporarily unavailable (os error 11)`
- `fork: retry: Resource temporarily unavailable`
- rustc panic: `failed to spawn helper thread (WouldBlock)`
- rustc panic: `Unable to install ctrlc handler: ... WouldBlock (Resource temporarily unavailable)`
- rustc panic: `called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }`

These are typically **environment/resource-limit** issues, not code bugs. The best remediation is to:

1. Wait with backoff (a few seconds → tens of seconds)
2. Re-run the command (still using `safe-run.sh`)

### Cargo config (`.cargo/config.toml`)

This repo tracks `.cargo/config.toml` for the `cargo xtask` alias, and it is kept intentionally minimal so CI isn't affected by agent-only settings.

Recommended memory-friendly Cargo settings live in environment variables (next section), not in the repo-tracked Cargo config.

### Environment (source `scripts/agent-env.sh`)

```bash
# Rust
# Cargo parallelism is defaulted to `-j1` for reliability in constrained sandboxes.
# Override by setting `AERO_CARGO_BUILD_JOBS` before sourcing `scripts/agent-env.sh`.
export CARGO_BUILD_JOBS=1
export RUSTC_WORKER_THREADS=1   # Limit rustc internal worker threads (reliability under contention)
export RAYON_NUM_THREADS=1      # Keep rayon pools aligned with Cargo parallelism
export RUST_TEST_THREADS=1      # Limit libtest parallelism (helps when per-user thread limits are tight)
export NEXTEST_TEST_THREADS=1   # Limit cargo-nextest test concurrency (if using cargo nextest)
export AERO_TOKIO_WORKER_THREADS=1  # Limit Tokio runtime worker threads for supported Aero binaries
export CARGO_INCREMENTAL=1

# Node (if running JS/TS tooling)
export NODE_OPTIONS="--max-old-space-size=4096"

# If your environment doesn't have the repo's pinned Node version from `.nvmrc`,
# you can bypass the hard error (it will still warn):
export AERO_ALLOW_UNSUPPORTED_NODE=1

# Playwright (if running browser tests)
export PW_TEST_WORKERS=1
```

---

## Timeouts (Non-Negotiable)

**Every command gets a timeout. No exceptions.**

Processes hang. Network calls block. Locks deadlock. Without timeouts, a single stuck process can block your entire session indefinitely.

```bash
# NEVER do this:
cargo build --locked  # Can hang forever

# ALWAYS do this (use safe-run.sh for both timeout + memory limit):
bash ./scripts/safe-run.sh cargo build --locked

# Or just timeout:
timeout -k 10 600 cargo build --locked
bash ./scripts/with-timeout.sh 600 cargo build --locked
```

### The `-k` flag is critical

**Always use `timeout -k <grace>` — never bare `timeout`.**

Misbehaving code can ignore SIGTERM indefinitely. The `-k 10` sends SIGKILL 10 seconds after SIGTERM if the process is still running.

```bash
# CORRECT — SIGKILL after 10s grace period:
timeout -k 10 600 cargo build --locked

# WRONG — process can ignore SIGTERM forever:
timeout 600 cargo build --locked
```

### Recommended Timeouts

| Operation | Timeout | Rationale |
|-----------|---------|-----------|
| `cargo build` (debug) | 10 min | Should complete in 2-5 min normally |
| `cargo build --release` | 20 min | LTO/optimization takes longer |
| `cargo test` | 10 min | Tests shouldn't take forever |
| `npm install` | 5 min | Network can be slow, but not infinite |
| `npm run build` | 10 min | Bundling is finite |
| Playwright tests | 5 min per test | Browser can hang |
| Any network request | 30 sec | DNS/connect/read timeouts |

### What Happens on Timeout

1. SIGTERM sent to the process
2. 10 second grace period for cleanup
3. SIGKILL if still running (non-negotiable)

**If something times out, it's a bug or a hang.** Investigate—don't just increase the timeout.

---

## Killing Processes: Do It Right

When something goes wrong, kill it properly. Half-killed processes are worse than running processes.

### Kill a Process Group (Preferred)

```bash
# Kill the entire process tree, not just the parent
kill -TERM -$PGID    # SIGTERM to process group
sleep 2
kill -KILL -$PGID    # SIGKILL if still alive
```

### Find and Kill Orphans

  ```bash
  # Find processes using excessive memory
  ps aux --sort=-%mem | head -20

  # Find your orphaned cargo/rustc processes
  pgrep -u $(whoami) -f 'cargo|rustc|node|chrome' | xargs -r ps -p

  # Kill a specific PID (choose from the pgrep output above)
  kill -TERM <PID>
  sleep 2
  kill -KILL <PID>
  ```

### Clean Up Lock Files

Crashed processes leave locks. Remove them:

```bash
# Cargo build directory lock
rm -f target/.cargo-lock

# npm lock
rm -f package-lock.json.lock node_modules/.package-lock.json

# OPFS/IndexedDB (browser storage) - clear via browser devtools or fresh profile
```

---

## Validating Outputs (Trust Nothing)

**Exit code 0 does not mean success.** Verify:

```bash
# BAD: Assumes success
cargo build --locked
./target/debug/mybin

# GOOD: Verify the artifact exists and is valid
cargo build --locked
if [[ ! -x ./target/debug/mybin ]]; then
    echo "ERROR: Build claimed success but binary missing" >&2
    exit 1
fi
./target/debug/mybin --version || { echo "Binary crashes on --version" >&2; exit 1; }
```

### Common "Successful Failures"

| Tool | Silent Failure Mode | How to Detect |
|------|---------------------|---------------|
| `cargo build` | Partial build, missing artifact | Check file exists + is executable |
| `wasm-pack` | Missing `.wasm` file | Check `pkg/*.wasm` exists |
| `npm run build` | Empty dist folder | Check `dist/` is non-empty |
| `cargo test` | Some tests skipped silently | Parse test output for skip count |
| `playwright test` | Flaky pass after retries | Check retry count in output |

---

## What NOT to Worry About

- **CPU contention**: The scheduler handles this. Don't reduce parallelism purely due to CPU contention — but note some agent sandboxes have low thread/process limits; `scripts/agent-env.sh` defaults to `CARGO_BUILD_JOBS=1` for stability (override via `AERO_CARGO_BUILD_JOBS`).
- **Disk I/O**: NVMe + Linux I/O scheduler handles contention fine. No need for `ionice` or I/O limits.
- **Disk space**: 110 TB is plenty. Clean up your target dirs occasionally but don't stress.
- **Network**: Not a factor for local development.

---

## When Memory Spikes Happen

Common memory-hungry operations:


| Operation               | Typical Peak | Mitigation                          |
| ----------------------- | ------------ | ----------------------------------- |
| `cargo build --release --locked` | 8-16 GB      | cap Cargo parallelism (agent default: `CARGO_BUILD_JOBS=1`) + memory limit |
| `cargo build --locked` (debug)   | 4-8 GB       | Usually fine                        |
| `wasm-pack build`       | 4-8 GB       | Usually fine                        |
| Playwright + Chrome     | 2-4 GB       | `PW_TEST_WORKERS=1`                 |
| `cargo doc`             | 4-8 GB       | Run alone if needed                 |
| Linking large binaries  | 4-8 GB       | lower codegen parallelism (`-C codegen-units=1`) helps |


If you're doing something unusual (like building with `-j16`), wrap it in a memory limit.

---

## Troubleshooting

### rustc fails to spawn helper threads ("Resource temporarily unavailable")

In heavily constrained sandboxes (especially when building `wasm32-unknown-unknown` + `wasm-threaded`), Rust may fail to create its internal thread pools and ICE with errors like:

```text
failed to spawn helper thread: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
```

Or it may surface as a panic/unwrap error (newer rustc versions):

```text
thread 'rustc' panicked at 'called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }'
```

Mitigations (start with the top-most):

```bash
# Limit how many rustc processes Cargo runs in parallel.
export CARGO_BUILD_JOBS=1

# rustc uses Rayon internally; keep its pool small as well.
export RAYON_NUM_THREADS=1

# If you still hit thread-spawn failures in debug/dev builds, also reduce per-crate
# codegen parallelism (especially helpful for threaded wasm builds).
export CARGO_PROFILE_DEV_CODEGEN_UNITS=1
```

Example (threaded WASM dev build of the core package):

```bash
CARGO_BUILD_JOBS=1 RAYON_NUM_THREADS=1 CARGO_PROFILE_DEV_CODEGEN_UNITS=1 \
  node web/scripts/build_wasm.mjs threaded dev --packages core
```

### Build was killed unexpectedly

Probably OOM. Check with:

```bash
dmesg | tail -20 | grep -i oom
```

Retry with:

```bash
bash ./scripts/run_limited.sh --as 12G -- cargo build --locked
```

### Cargo says "Blocking waiting for file lock on package cache"

If `cargo` appears stuck and repeatedly prints:

```
Blocking waiting for file lock on package cache
```

it usually means another `cargo` process on the shared host is currently
updating/using the global Cargo registry cache (common when many agents run
builds concurrently). This is typically transient: once the other `cargo`
finishes its download/unpack step, the lock is released and your command
continues.

Mitigations:

- **Wait** (best default). The lock should clear on its own.
- **Stagger heavy Cargo runs** across agents (avoid everyone starting a fresh
  build at once).
- If you need full isolation, run with a **per-checkout `CARGO_HOME`** (at the
  cost of duplicating cache data / doing your own downloads):

  ```bash
  CARGO_HOME="$PWD/.cargo-home" cargo build --locked
  ```

  If you are running via `safe-run.sh`, you can opt into the same behavior without
  sourcing `scripts/agent-env.sh`:

  ```bash
  AERO_ISOLATE_CARGO_HOME=1 bash ./scripts/safe-run.sh cargo build --locked
  ```

  If you are using the agent env helper, you can opt into the same behavior:

  ```bash
  export AERO_ISOLATE_CARGO_HOME=1
  # Or pick a custom directory:
  # export AERO_ISOLATE_CARGO_HOME="/tmp/aero-cargo-home"
  source ./scripts/agent-env.sh
  ```

  Note: this intentionally overrides any existing `CARGO_HOME` so the isolation
  actually takes effect.

### Cargo says "Blocking waiting for file lock on build directory"

If `cargo` prints:

```
Blocking waiting for file lock on build directory
```

it means **another Cargo process is currently using this checkout's build output directory** (typically `target/`).
This is different from the package cache lock above (which is about `CARGO_HOME` / registry state shared across
agents).

Common causes:

- You started two `cargo` commands in parallel in the same checkout.
- An IDE/background task is running `cargo check` continuously.
- A previous `cargo` invocation is still running (or got stuck) in this repo.

Mitigations:

- **Wait** for the other build to finish (best default).
- If you need to run multiple builds concurrently, use a **separate `CARGO_TARGET_DIR`** per command:

  ```bash
  CARGO_TARGET_DIR="$PWD/target-alt" cargo test --locked -p <crate>
  ```

  (This uses more disk space, but avoids the lock contention.)

### rustc wrapper errors (`sccache`, etc)

Some environments configure a rustc wrapper (most commonly `sccache`) either via `~/.cargo/config.toml`:

```toml
[build]
rustc-wrapper = "sccache"
```

or via environment variables (`RUSTC_WRAPPER=sccache`, `RUSTC_WORKSPACE_WRAPPER=sccache`, etc).

If the wrapper daemon/socket is unhealthy, Cargo can fail with errors like:

```
sccache: error: failed to execute compile
```

Mitigations:

- **Disable wrappers for the command**:
  ```bash
  RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= \
    CARGO_BUILD_RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER= \
    cargo test --locked
  ```
- Or, when using `safe-run.sh` (recommended), note that it clears *environment-based* `sccache`
  wrappers by default. To force-disable wrappers (including those injected via Cargo config),
  set:
  ```bash
  AERO_DISABLE_RUSTC_WRAPPER=1 bash ./scripts/safe-run.sh cargo test --locked
  ```
- Or, when using the agent env helper:
  ```bash
  export AERO_DISABLE_RUSTC_WRAPPER=1
  source ./scripts/agent-env.sh
  ```

Notes:

- By default, `scripts/safe-run.sh` and `scripts/agent-env.sh` clear *only* `sccache` wrappers they
  see in the environment and preserve non-sccache wrappers (e.g. `ccache`). Use
  `AERO_DISABLE_RUSTC_WRAPPER=1` to force-disable all wrappers.

### Build is very slow

You might be over-constrained. `scripts/agent-env.sh` defaults to `-j1` for reliability in constrained sandboxes; if your environment can handle more parallelism, override it:

```bash
export AERO_CARGO_BUILD_JOBS=2  # or 4, etc
source ./scripts/agent-env.sh
echo $CARGO_BUILD_JOBS
```

### rustc panics with "failed to spawn helper thread" / "Resource temporarily unavailable"

In heavily contended agent sandboxes, `rustc` (or the linker driver it spawns) can fail with errors like:

- `failed to spawn helper thread (WouldBlock)`
- `failed to spawn work thread: Resource temporarily unavailable`
- `called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }`
- `could not exec the linker \`cc\`: Resource temporarily unavailable`

These are usually **transient shared-host resource issues** (PID/thread limits), not deterministic build failures.

Mitigations:

- **Wait and retry** (best default).
- Ensure you're using minimal parallelism (`-j1` is the agent default):
  ```bash
  AERO_CARGO_BUILD_JOBS=1 bash ./scripts/safe-run.sh cargo test --locked
  ```
- If it still happens, reduce per-crate codegen parallelism further:
  ```bash
  AERO_RUST_CODEGEN_UNITS=1 bash ./scripts/safe-run.sh cargo test --locked
  # (alias): AERO_CODEGEN_UNITS=1 bash ./scripts/safe-run.sh cargo test --locked
  ```

### "Too many open files"

```bash
ulimit -n 4096
```

---

## Helper Scripts

The `scripts/` directory contains:


| Script               | Purpose                                           |
| -------------------- | ------------------------------------------------- |
| `safe-run.sh`        | **Recommended**: Run with both timeout + memory limit |
| `run_limited.sh`     | Run a command with RLIMIT_AS memory limit         |
| `with-timeout.sh`    | Run a command with a timeout (uses `-k` for SIGKILL) |
| `agent-env.sh`       | Source this to set recommended env vars           |
| `agent-env-setup.sh` | One-time sanity checks + environment validation   |


### Quick Reference

```bash
# One-time setup (validates environment, shows warnings)
bash ./scripts/agent-env-setup.sh

# Activate environment in current shell
source ./scripts/agent-env.sh

# Run a build with full protection (RECOMMENDED)
bash ./scripts/safe-run.sh cargo build --release --locked

# Override defaults
AERO_TIMEOUT=1200 AERO_MEM_LIMIT=16G bash ./scripts/safe-run.sh cargo build --release --locked

# Just timeout (always use -k for SIGKILL fallback!)
timeout -k 10 600 cargo test --locked
bash ./scripts/with-timeout.sh 600 cargo test --locked

# Just memory limit (RLIMIT_AS)
bash ./scripts/run_limited.sh --as 12G -- cargo build --release --locked
```


---

## Headless / GPU-less Development (EC2, etc.)

Developing on headless GPU-less VMs (typical EC2 instances) works fine for most tasks. Here's what to know:

### Works perfectly

- Rust/WASM compilation
- All CPU emulator tests
- WASM SIMD (uses CPU SSE/AVX)
- SharedArrayBuffer / COOP/COEP (just HTTP headers)
- Storage, networking, audio subsystem tests
- Most Playwright tests (headless Chrome/Firefox)

### Friction: WebGPU is unavailable

On GPU-less systems, `navigator.gpu` is typically undefined even in headless Chrome. This means:

- **WebGPU tests will skip** (not fail) by default
- **WebGL2 fallback tests still run** (software rendered via llvmpipe/SwiftShader)
- **GPU golden-image tests may differ** from hardware baselines

**This is fine for most development.** The test harness gates WebGPU:

```bash
# Default: WebGPU tests skip if unavailable (GPU-less friendly)
npm run test:e2e

# Force WebGPU requirement (will fail on GPU-less systems)
AERO_REQUIRE_WEBGPU=1 npm run test:e2e
```

### What you can't test locally on GPU-less

- WebGPU render correctness (pixel-perfect output)
- WebGPU performance characteristics
- GPU timestamp queries

**Workaround:** Push to a branch and let CI with GPU runners validate, or use a GPU-equipped dev machine for graphics work.

### Software rendering (WebGL2)

WebGL2 works via software rendering (llvmpipe), but it's slow:

```bash
# These flags help headless Chrome use software GL
export PLAYWRIGHT_CHROMIUM_ARGS="--disable-gpu --use-gl=swiftshader"
```

Functional tests pass; performance is not representative.

### Summary for GPU-less development


| Task                 | Works?             |
| -------------------- | ------------------ |
| Build and compile    | ✅ Yes              |
| CPU emulator tests   | ✅ Yes              |
| Playwright (non-GPU) | ✅ Yes              |
| WebGL2 smoke tests   | ✅ Yes (slow)       |
| WebGPU tests         | ⚠️ Skip by default |
| GPU perf benchmarks  | ❌ Meaningless      |


---

## Safe Command Execution Patterns

### The Fully Defensive Pattern

For any non-trivial command:

```bash
#!/bin/bash
set -euo pipefail

TIMEOUT=600
MEM_LIMIT=12G
CMD=(cargo build --release --locked)

echo "[run] Starting: ${CMD[*]}"
echo "[run] Timeout: ${TIMEOUT}s, Memory limit: $MEM_LIMIT"

# Capture both stdout and stderr, with timeout and memory limit
if ! AERO_TIMEOUT="$TIMEOUT" AERO_MEM_LIMIT="$MEM_LIMIT" bash ./scripts/safe-run.sh "${CMD[@]}" 2>&1 | tee build.log; then
    echo "[run] FAILED: ${CMD[*]}" >&2
    echo "[run] Last 50 lines of output:" >&2
    tail -50 build.log >&2
    exit 1
fi

# Verify output exists
if [[ ! -f target/release/aero ]]; then
    echo "[run] ERROR: Command succeeded but expected output missing" >&2
    exit 1
fi

echo "[run] SUCCESS: ${CMD[*]}"
```

### Quick Defensive One-Liners

```bash
# Build with all protections
AERO_TIMEOUT=600 AERO_MEM_LIMIT=12G bash ./scripts/safe-run.sh cargo build --locked 2>&1 | tee build.log

# Test with timeout (tests should be fast)
bash ./scripts/with-timeout.sh 300 cargo test --locked 2>&1 | tee test.log

# npm with timeout (network can hang)
bash ./scripts/with-timeout.sh 300 npm ci 2>&1 | tee npm.log
```

### Recovering from Failures

When something fails:

1. **Check what's still running:**
   ```bash
   pgrep -u $(whoami) -af 'cargo|rustc|node|npm|chrome'
   ```

2. **Kill orphans:**
   ```bash
   # Use the output from step 1; be intentional and kill specific PIDs.
   kill -TERM <PID>
   sleep 2
   kill -KILL <PID>
   ```

3. **Clean corrupted state:**
   ```bash
   rm -rf target/.cargo-lock
   cargo clean -p <crate-that-failed>
   ```

4. **Retry with more visibility:**
   ```bash
   RUST_BACKTRACE=1 cargo build --locked -vv 2>&1 | tee verbose-build.log
   ```

---

## Windows 7 Test ISO

A Windows 7 Professional x64 ISO is available at:

```
/state/win7.iso
```

Use this for integration testing once the emulator can boot. Do not redistribute.

---

## Summary

1. **Assume hostility** — every process can hang, OOM, or misbehave
2. **Memory is the hard constraint** — use `run_limited.sh`/`safe-run.sh` for heavy builds
3. **Timeouts are mandatory** — no command runs without a deadline
4. **Verify outputs** — exit code 0 doesn't mean success
5. **Kill aggressively** — SIGTERM, wait, SIGKILL; clean up orphans
6. **Tune parallelism intentionally** — agent-env defaults to `-j1` for stability; increase via `AERO_CARGO_BUILD_JOBS` if your sandbox allows it
7. **GPU-less is fine** — WebGPU tests skip gracefully, WebGL2 works via software
