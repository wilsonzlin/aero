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
systemd-run --user --scope -p MemoryMax=12G cargo build --release

# Or use the helper script
./scripts/mem-limit.sh 12G cargo build --release
```

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

# Playwright (if running browser tests)
export PW_TEST_WORKERS=1
```

---

## Timeouts

Long-running commands should have timeouts to catch infinite loops or stuck processes:

```bash
# 10 minute timeout for builds
timeout 600 cargo build

# 5 minute timeout for tests  
timeout 300 cargo test

# Helper script with graceful shutdown
./scripts/with-timeout.sh 600 cargo build --release
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
| `cargo build --release` | 8-16 GB      | `CARGO_BUILD_JOBS=4` + memory limit |
| `cargo build` (debug)   | 4-8 GB       | Usually fine                        |
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
./scripts/mem-limit.sh 12G cargo build
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
