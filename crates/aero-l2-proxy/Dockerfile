# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY tools ./tools
COPY emulator ./emulator
COPY drivers ./drivers

RUN cargo build --release --locked -p aero-l2-proxy

FROM debian:bookworm-slim AS runtime

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

EXPOSE 8090

USER aero

HEALTHCHECK --interval=10s --timeout=2s --start-period=5s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8090/healthz || exit 1

ENTRYPOINT ["aero-l2-proxy"]
