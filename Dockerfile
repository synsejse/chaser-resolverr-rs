FROM rust:1-bookworm AS base
RUN cargo install cargo-chef --version ^0.1

FROM base AS planner
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef prepare --recipe-path recipe.json

FROM base AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release

# The headful browser lives in the chaser-cf sidecar; this image just
# ships the Rust binary plus CA certs for outbound TLS to chaser-cf.
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      dumb-init \
      && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/chaser-resolverr-rs /usr/local/bin/chaser-resolverr-rs

EXPOSE 8191

RUN mkdir -p /data
VOLUME ["/data"]

ENV RUST_LOG=info,tracing::span=warn

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8191/health > /dev/null || exit 1

ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD ["/usr/local/bin/chaser-resolverr-rs"]
