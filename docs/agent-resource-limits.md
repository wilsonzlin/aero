# Agent Resource Limits & Concurrency Guide

**Audience:** Coding agents developing Aero (not end-users or CI runners).

**Goal:** Maximum speed without OOM-killing the shared host.

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
./scripts/agent-env-setup.sh
```

Then activate the recommended environment in your current shell:

```bash
source ./scripts/agent-env.sh
```

---

## Memory Limit Enforcement

### Option 1: systemd-run (Recommended)

Wrap long-running or memory-intensive commands:

```bash
# For cargo build (can spike to 16GB without limits)
systemd-run --user --scope -p MemoryMax=12G cargo build --release --locked

# Or use the helper script
./scripts/mem-limit.sh 12G cargo build --release --locked
```

`mem-limit.sh` prefers `systemd-run` when available, and falls back to
`prlimit`/`ulimit` on systems without a working systemd user session (common in
containers).

### Option 2: Cgroup (if you have root/sudo)

```bash
# Create a memory-limited slice for this shell and all children
sudo systemd-run --scope -p MemoryMax=12G --uid=$(id -u) bash
# Now all commands in this shell respect the 12G limit
```

### Option 3: Environment Variable (Softer)

This doesn't enforce a hard limit but reduces peak usage:

```bash
export CARGO_BUILD_JOBS=4       # Limit parallel rustc (default: num_cpus)
export RUSTFLAGS="-C codegen-units=4"  # Reduce per-crate parallelism
```

---

## Recommended Build Settings

These balance speed with reasonable memory usage. They're defaults, not hard constraints—override if you know what you're doing.

### Cargo config (`.cargo/config.toml`)

This repo tracks `.cargo/config.toml` for the `cargo xtask` alias, and it is kept intentionally minimal so CI isn't affected by agent-only settings.

Recommended memory-friendly Cargo settings live in environment variables (next section), not in the repo-tracked Cargo config.

### Environment (source `scripts/agent-env.sh`)

```bash
# Rust
export CARGO_BUILD_JOBS=4
export RUSTFLAGS="-C codegen-units=4"
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

## Timeouts

Long-running commands should have timeouts to catch infinite loops or stuck processes:

```bash
# 10 minute timeout for builds
timeout 600 cargo build --locked

# 5 minute timeout for tests  
timeout 300 cargo test --locked

# Helper script with graceful shutdown
./scripts/with-timeout.sh 600 cargo build --release --locked
```

---

## What NOT to Worry About

- **CPU contention**: The scheduler handles this. Don't use `-j1` out of excessive caution.
- **Disk I/O**: NVMe + Linux I/O scheduler handles contention fine. No need for `ionice` or I/O limits.
- **Disk space**: 110 TB is plenty. Clean up your target dirs occasionally but don't stress.
- **Network**: Not a factor for local development.

---

## When Memory Spikes Happen

Common memory-hungry operations:


| Operation               | Typical Peak | Mitigation                          |
| ----------------------- | ------------ | ----------------------------------- |
| `cargo build --release --locked` | 8-16 GB      | `CARGO_BUILD_JOBS=4` + memory limit |
| `cargo build --locked` (debug)   | 4-8 GB       | Usually fine                        |
| `wasm-pack build`       | 4-8 GB       | Usually fine                        |
| Playwright + Chrome     | 2-4 GB       | `PW_TEST_WORKERS=1`                 |
| `cargo doc`             | 4-8 GB       | Run alone if needed                 |
| Linking large binaries  | 4-8 GB       | `-C codegen-units=4` helps          |


If you're doing something unusual (like building with `-j16`), wrap it in a memory limit.

---

## Troubleshooting

### Build was killed unexpectedly

Probably OOM. Check with:

```bash
dmesg | tail -20 | grep -i oom
```

Retry with:

```bash
./scripts/mem-limit.sh 12G cargo build --locked
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

### sccache errors ("failed to execute compile")

Some environments configure a global Cargo rustc wrapper (commonly `sccache`) via `~/.cargo/config.toml`:

```toml
[build]
rustc-wrapper = "sccache"
```

If the `sccache` daemon/socket is unhealthy, Cargo can fail with errors like:

```
sccache: error: failed to execute compile
```

Mitigations:

- **Disable wrappers for the command**:
  ```bash
  RUSTC_WRAPPER= cargo test --locked
  ```
- Or, when using the agent env helper:
  ```bash
  export AERO_DISABLE_RUSTC_WRAPPER=1
  source ./scripts/agent-env.sh
  ```

### Build is very slow

You might be over-constrained. Check if you're accidentally running with `-j1`:

```bash
echo $CARGO_BUILD_JOBS  # Should be 4 or unset, not 1
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
| `agent-env.sh`       | Source this to set recommended env vars           |
| `agent-env-setup.sh` | One-time sanity checks (does not overwrite repo Cargo config) |
| `mem-limit.sh`       | Run a command with a memory limit                 |
| `with-timeout.sh`    | Run a command with a timeout                      |


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

## Summary

1. **Memory is the only hard constraint** — use `mem-limit.sh` or `systemd-run` for heavy builds
2. **Don't over-constrain** — `-j4` is fine, `-j1` is too conservative
3. **Timeouts for long commands** — catch runaway processes
4. **Everything else is fine** — let the scheduler do its job
5. **GPU-less is fine** — WebGPU tests skip gracefully, WebGL2 works via software
