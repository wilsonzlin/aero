# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY server ./server
COPY tools ./tools

RUN cargo build --release --locked -p aero-storage-server

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

COPY --from=builder /workspace/target/release/aero-storage-server /usr/local/bin/aero-storage-server

ENV AERO_STORAGE_LISTEN_ADDR=0.0.0.0:8080
ENV AERO_STORAGE_IMAGE_ROOT=/images

EXPOSE 8080

USER aero

# Healthcheck must hit the HTTP endpoint.
HEALTHCHECK --interval=10s --timeout=2s --start-period=5s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

ENTRYPOINT ["aero-storage-server"]
