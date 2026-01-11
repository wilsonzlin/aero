# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.92.0
ARG GIT_SHA=dev
ARG VERSION=$GIT_SHA
ARG BUILD_TIMESTAMP=unknown
FROM rust:${RUST_VERSION}-bookworm AS builder

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates
COPY tools ./tools
COPY emulator ./emulator
COPY drivers ./drivers
COPY xtask ./xtask

RUN cargo build --release --locked -p aero-l2-proxy

FROM debian:bookworm-slim AS runtime

ARG GIT_SHA=dev
ARG VERSION=$GIT_SHA
ARG BUILD_TIMESTAMP=unknown

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Non-root runtime user.
RUN useradd \
    --uid 10001 \
    --create-home \
    --home-dir /home/aero \
    --shell /usr/sbin/nologin \
    aero

WORKDIR /app

COPY --from=builder /workspace/target/release/aero-l2-proxy /usr/local/bin/aero-l2-proxy

ENV AERO_L2_PROXY_LISTEN_ADDR=0.0.0.0:8090
ENV AERO_L2_PROXY_VERSION=$VERSION
ENV AERO_L2_PROXY_GIT_SHA=$GIT_SHA
ENV AERO_L2_PROXY_BUILD_TIMESTAMP=$BUILD_TIMESTAMP

EXPOSE 8090

USER aero

HEALTHCHECK --interval=10s --timeout=2s --start-period=5s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8090/healthz || exit 1

ENTRYPOINT ["aero-l2-proxy"]
