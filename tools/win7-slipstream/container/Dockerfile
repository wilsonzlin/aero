FROM rust:1.74-bookworm AS build

ARG DEBIAN_FRONTEND=noninteractive

# Minimal build deps for typical Rust crates.
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    pkg-config \
    libssl-dev \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# NOTE: This Dockerfile is intended to be built with a build context that
# contains the `aero-win7-slipstream` Rust crate.
# (e.g. `docker build -f tools/win7-slipstream/container/Dockerfile .`)
COPY . .

ARG CARGO_MANIFEST_PATH=tools/win7-slipstream/Cargo.toml
ARG BIN_NAME=aero-win7-slipstream

ENV CARGO_TARGET_DIR=/tmp/target
RUN cargo build --release --locked --manifest-path "${CARGO_MANIFEST_PATH}"
RUN install -Dm755 "/tmp/target/release/${BIN_NAME}" "/out/${BIN_NAME}"

FROM debian:bookworm-slim AS runtime

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    xorriso \
    p7zip-full \
    wimtools \
    libhivex-bin \
  && rm -rf /var/lib/apt/lists/*

# Run as non-root by default; callers can override with `docker run --user`.
RUN useradd --create-home --uid 1000 --shell /usr/sbin/nologin slipstream \
  && mkdir -p /work \
  && chown slipstream:slipstream /work

COPY --from=build /out/aero-win7-slipstream /usr/local/bin/aero-win7-slipstream

ENV HOME=/home/slipstream

USER slipstream
WORKDIR /work

ENTRYPOINT ["/usr/local/bin/aero-win7-slipstream"]
CMD ["--help"]
