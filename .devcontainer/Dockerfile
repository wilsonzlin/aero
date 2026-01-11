# syntax=docker/dockerfile:1

FROM mcr.microsoft.com/devcontainers/base:ubuntu-22.04

ARG NODE_VERSION=20.11.1

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

# Native tooling for building + testing:
# - Rust (stable + nightly + wasm target + rust-src)
# - Node (pinned to the same version as CI)
# - wasm-pack + wasm-opt (Binaryen)
# - QEMU + boot test deps (mtools, nasm, unzip)
# - ACPICA tools (iasl)
# - Playwright runtime deps (browser binaries installed separately via `npx playwright install`)
RUN apt-get update \
  && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    build-essential \
    pkg-config \
    cmake \
    clang \
    lld \
    python3 \
    unzip \
    xz-utils \
    # Boot / firmware tests
    qemu-system-x86 \
    qemu-utils \
    mtools \
    nasm \
    # ACPI validation
    acpica-tools \
    # wasm-opt
    binaryen \
    # Playwright deps (Ubuntu)
    libnss3 \
    libatk1.0-0 \
    libatk-bridge2.0-0 \
    libatspi2.0-0 \
    libdrm2 \
    libxkbcommon0 \
    libxcomposite1 \
    libxdamage1 \
    libxfixes3 \
    libxrandr2 \
    libgbm1 \
    libasound2 \
    libcups2 \
    libdbus-1-3 \
    libgtk-3-0 \
    libpango-1.0-0 \
    libpangocairo-1.0-0 \
    libcairo2 \
    libx11-xcb1 \
    libxcb-dri3-0 \
    libxshmfence1 \
    xvfb \
    fonts-liberation \
  && rm -rf /var/lib/apt/lists/*

RUN arch="$(dpkg --print-architecture)" \
  && case "${arch}" in \
    amd64) node_arch="x64" ;; \
    arm64) node_arch="arm64" ;; \
    *) echo "Unsupported architecture: ${arch}" >&2; exit 1 ;; \
  esac \
  && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${node_arch}.tar.xz" -o /tmp/node.tar.xz \
  && tar -xJf /tmp/node.tar.xz --strip-components=1 -C /usr/local \
  && rm /tmp/node.tar.xz \
  && node --version \
  && npm --version

USER vscode
ENV PATH="/home/vscode/.cargo/bin:${PATH}"

COPY --chown=vscode:vscode rust-toolchain.toml scripts/toolchains.json /tmp/aero-toolchains/

RUN stable_toolchain="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]\+\)".*/\1/p' /tmp/aero-toolchains/rust-toolchain.toml | head -n1)" \
  && wasm_nightly="$(node -p "require('/tmp/aero-toolchains/toolchains.json').rust.nightlyWasm")" \
  && if [[ -z "${stable_toolchain}" ]]; then echo "error: unable to determine pinned stable toolchain from rust-toolchain.toml" >&2; exit 1; fi \
  && if [[ -z "${wasm_nightly}" ]]; then echo "error: unable to determine rust.nightlyWasm from scripts/toolchains.json" >&2; exit 1; fi \
  && curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "${stable_toolchain}" \
  && rustup toolchain install "${wasm_nightly}" --profile minimal \
  && rustup target add wasm32-unknown-unknown --toolchain "${stable_toolchain}" \
  && rustup target add wasm32-unknown-unknown --toolchain "${wasm_nightly}" \
  && rustup component add rustfmt clippy --toolchain "${stable_toolchain}" \
  && rustup component add rust-src --toolchain "${wasm_nightly}"

RUN cargo install just --locked

# Prefer the upstream installer (prebuilt binaries) over compiling from crates.io.
RUN curl -fsSL https://rustwasm.github.io/wasm-pack/installer/init.sh | bash
