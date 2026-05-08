# syntax=docker/dockerfile:1

FROM rust:1.91-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && \
    cp /app/target/release/pay3 /usr/local/bin/pay3

FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --system --gid 10001 pay3 && \
    useradd --system --uid 10001 --gid pay3 --home-dir /var/lib/pay3 pay3 && \
    mkdir -p /var/lib/pay3 && \
    chown -R pay3:pay3 /var/lib/pay3

COPY --from=builder /usr/local/bin/pay3 /usr/local/bin/pay3

ENV APP_BIND=0.0.0.0:3000 \
    KVDB_PATH=/var/lib/pay3/pay3.redb

WORKDIR /var/lib/pay3
USER pay3:pay3

EXPOSE 3000
VOLUME ["/var/lib/pay3"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3000/readyz >/dev/null || exit 1

ENTRYPOINT ["pay3"]
